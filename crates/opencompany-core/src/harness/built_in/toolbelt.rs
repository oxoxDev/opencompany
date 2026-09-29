//! Coding + web tool wiring for embedded company agents (Cell A).
//!
//! This module bridges a curated slice of OpenHuman's tool surface into the
//! harness's per-agent [`AgentBuilder`](openhuman_core::agent::AgentBuilder)
//! wiring in [`build`](crate::harness::build). Where [`file_tools`] grants an
//! agent read/write inside its own workspace, this module adds the **exec-grade**
//! families behind their own grant namespaces:
//!
//! * **`shell`** → `shell` (run commands) + `read_workspace_state`.
//! * **`code`** → `apply_patch`, `git_operations`, `csv_export`.
//! * **`web`** → `web_fetch`, `http_request`, `curl`, `image_info`.
//! * **`subagent`** → reserved, **empty in v1** (see [`subagent_tools`]).
//!
//! Everything here is scoped **per company/agent workspace** — no
//! process-global state — so parallel tenants stay isolated:
//!
//! * Shell + code tools share one [`SecurityPolicy`] built by [`exec_security`],
//!   pinned to the agent's workspace with `workspace_only`, high-risk commands
//!   blocked, tool-install denied, and an autonomy level mapped from the
//!   company's [`PolicyMode`] by [`autonomy_for`] — no longer 1:1 since `auto`
//!   (issue #560) has no upstream counterpart and borrows `Supervised`. This is
//!   the *strict* policy — opencompany's own
//!   [`ApprovalPolicy`](crate::harness::policy::ApprovalPolicy) tool policy stays
//!   the real fail-closed approval gate on top of it, **except** on the workflow
//!   `tool_call` path, where no such gate is installed and this policy is the
//!   whole tier. See [`autonomy_for`].
//! * Shell command audit is keyed on a per-agent, **host-owned** sink directory
//!   ([`shell_audit`]) — `companies/<slug>/audit/<agent>/`, deliberately outside
//!   the agent's own workspace — so audit trails never cross tenants *and* the
//!   agent's sanctioned write paths cannot reach the record of what it did
//!   (issue #775). The shell tool itself is wrapped by
//!   [`AuditedShellTool`](crate::harness::audit::AuditedShellTool), which
//!   appends the command's intent line *before* the command runs and refuses
//!   the call if that append fails.
//! * Web tools reuse OpenHuman's upstream SSRF guard (`url_guard`): every
//!   request is validated against the per-company allowlist AND has
//!   private/loopback/link-local/metadata IPs rejected — even in the default
//!   "allow all public hosts" mode (an empty allowlist). The guard is applied
//!   inside the tool constructors; it is not re-implemented here.
//!
//! A single [`filter_by_capabilities`] pass runs just before the tool vector is
//! handed to the builder. Today the only [`CapabilityFilter`] is
//! [`CapabilityFilter::AllowAll`] (identity); a future capability-tier cell only
//! swaps how the filter is constructed. Tools with no mapped namespace
//! (memory, MCP, orchestrator, skills) are **intrinsic** and always kept.
//!
//! **Admitted since v1** — `search` (issue #238) is no longer deferred. It was
//! held back for one *infrastructure* reason ("need engine keys"), which the
//! managed-platform-credential pattern from #109 dissolved: the backend proxies
//! the search and bills the platform, so there is no engine key to hold. It
//! lives in [`search`](crate::harness::search) rather than here because it is a
//! priced backend call rather than a local exec tool, but it is namespaced by
//! [`namespace_of`] like any other gateable family.
//!
//! **Still deferred** (need infrastructure not present in v1): browser
//! automation (needs a backend), Node/NPM exec (need a managed-runtime
//! bootstrap), and OpenHuman's sub-agent spawn tools (global registry + budget
//! bypass — unsafe under multi-tenancy). Those three were excluded for *safety*
//! reasons that still hold, which is exactly why search could move and they
//! cannot.
//!
//! **Contract — the dispatched company agent is a constrained, metered
//! derivative of an OpenHuman agent** (pinned by the contract tests in
//! [`build`](crate::harness::build)). A dispatched desk/roster agent receives
//! the curated exec subset above (`shell` / `code` / `web`) plus its intrinsic
//! memory / file / MCP / skill tools — and **nothing more**. Two invariants
//! hold for every dispatched agent and are locked by test so a future change
//! cannot silently widen or narrow the belt:
//!
//! * **Depth cap = 1 — no re-delegation.** The orchestrator's delegation tools
//!   (`query_company` / `spawn_task` / `delegate_to_desk`, plus the other
//!   orchestrator-only roster/workflow tools) are wired ONLY onto the company
//!   orchestrator; a dispatched agent never receives them, so a dispatched turn
//!   cannot fan work out further (the "no sub-agent re-delegation in v1"
//!   invariant, issue #178).
//! * **Deferred surfaces stay absent.** Raw browser automation, Node/NPM exec,
//!   OpenHuman sub-agent spawn tools (the `subagent` namespace is reserved but
//!   EMPTY in v1), skill *execution*, the raw memory-tree tool surface, and
//!   `forget` are all out of a dispatched belt. `web_search` (#238) is now
//!   admitted, but only under an **explicit** `search` grant plus a managed
//!   credential — so it is still absent from the `*`-granted belt the contract
//!   test pins.

use std::path::Path;
use std::sync::Arc;

use openhuman_core as oh;

use oh::agent::host_runtime::{NativeRuntime, RuntimeAdapter};
use oh::config::{AuditConfig, HttpRequestConfig};
use oh::security::{
    AuditLogger, AutonomyLevel, SecurityPolicy, get_or_create_workspace_audit_logger,
};
use oh::tools::{
    ApplyPatchTool, CurlTool, GitOperationsTool, HttpRequestTool, ImageInfoTool, WebFetchTool,
    WorkspaceStateTool,
};
use tinytools::Tool;

use crate::harness::policy::PolicyMode;

use tinytools::{
    PermissionLevel, ToolCallOptions, ToolCategory, ToolResult, ToolRunContext, ToolScope,
    ToolSpec, ToolTimeout,
};

trait ToolGuard: Send + Sync {
    fn refusal(&self, args: &serde_json::Value) -> Option<ToolResult>;

    fn timeout_policy(&self, inner: ToolTimeout) -> ToolTimeout {
        inner
    }
}

struct GuardedTool<T, G> {
    inner: T,
    guard: G,
}

#[async_trait::async_trait]
impl<T: Tool, G: ToolGuard> Tool for GuardedTool<T, G> {
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Some(refusal) = self.guard.refusal(&args) {
            return Ok(refusal);
        }
        self.inner.execute(args).await
    }

    async fn execute_with_options(
        &self,
        args: serde_json::Value,
        options: ToolCallOptions,
    ) -> anyhow::Result<ToolResult> {
        if let Some(refusal) = self.guard.refusal(&args) {
            return Ok(refusal);
        }
        self.inner.execute_with_options(args, options).await
    }

    async fn execute_with_context(
        &self,
        args: serde_json::Value,
        options: ToolCallOptions,
        context: Option<&dyn ToolRunContext>,
    ) -> anyhow::Result<ToolResult> {
        if let Some(refusal) = self.guard.refusal(&args) {
            return Ok(refusal);
        }
        self.inner
            .execute_with_context(args, options, context)
            .await
    }

    fn name(&self) -> &str {
        self.inner.name()
    }
    fn description(&self) -> &str {
        self.inner.description()
    }
    fn parameters_schema(&self) -> serde_json::Value {
        self.inner.parameters_schema()
    }
    fn spec(&self) -> ToolSpec {
        self.inner.spec()
    }
    fn supports_markdown(&self) -> bool {
        self.inner.supports_markdown()
    }
    fn permission_level(&self) -> PermissionLevel {
        self.inner.permission_level()
    }
    fn permission_level_with_args(&self, args: &serde_json::Value) -> PermissionLevel {
        self.inner.permission_level_with_args(args)
    }
    fn scope(&self) -> ToolScope {
        self.inner.scope()
    }
    fn category(&self) -> ToolCategory {
        self.inner.category()
    }
    fn is_concurrency_safe(&self, args: &serde_json::Value) -> bool {
        self.inner.is_concurrency_safe(args)
    }
    fn external_effect(&self) -> bool {
        self.inner.external_effect()
    }
    fn external_effect_with_args(&self, args: &serde_json::Value) -> bool {
        self.inner.external_effect_with_args(args)
    }
    fn host_extension(&self) -> Option<&(dyn std::any::Any + Send + Sync)> {
        self.inner.host_extension()
    }
    fn host_call_extension(
        &self,
        args: &serde_json::Value,
    ) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        self.inner.host_call_extension(args)
    }
    fn max_result_size_chars(&self) -> Option<usize> {
        self.inner.max_result_size_chars()
    }
    fn timeout_policy(&self, args: &serde_json::Value) -> ToolTimeout {
        self.guard.timeout_policy(self.inner.timeout_policy(args))
    }
    fn display_label(&self, args: &serde_json::Value) -> Option<String> {
        self.inner.display_label(args)
    }
    fn display_detail(&self, args: &serde_json::Value) -> Option<String> {
        self.inner.display_detail(args)
    }
}

/// Each export admits at most 100,000 rows, 16 MiB of JSON and 8 MiB of CSV.
struct CsvLimits;

const MAX_CSV_ROWS: usize = 100_000;
const MAX_CSV_INPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_CSV_BYTES: usize = 8 * 1024 * 1024;

type CsvExportTool = GuardedTool<oh::tools::CsvExportTool, CsvLimits>;

impl CsvExportTool {
    fn new(security: Arc<SecurityPolicy>) -> Self {
        Self {
            inner: oh::tools::CsvExportTool::new(security),
            guard: CsvLimits,
        }
    }
}

fn csv_cell_bytes(cell: &str) -> usize {
    let quoted = cell.contains([',', '"', '\n', '\r']);
    cell.len()
        + if quoted {
            2 + cell.bytes().filter(|&b| b == b'"').count()
        } else {
            0
        }
}

impl ToolGuard for CsvLimits {
    fn refusal(&self, args: &serde_json::Value) -> Option<ToolResult> {
        let data = args.get("data")?.as_str()?;
        if data.len() > MAX_CSV_INPUT_BYTES {
            return Some(ToolResult::error(format!(
                "CSV input exceeds {MAX_CSV_INPUT_BYTES} bytes"
            )));
        }
        let parsed: serde_json::Value = serde_json::from_str(data).ok()?;
        let rows = parsed.as_array()?;
        if rows.len() > MAX_CSV_ROWS {
            return Some(ToolResult::error(format!(
                "CSV export exceeds {MAX_CSV_ROWS} rows"
            )));
        }
        let columns: Vec<&str> = match args.get("columns").and_then(serde_json::Value::as_array) {
            Some(columns) => columns
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect(),
            None => rows
                .first()
                .and_then(serde_json::Value::as_object)
                .map(|object| object.keys().map(String::as_str).collect())
                .unwrap_or_default(),
        };
        let mut bytes = columns.len().max(1);
        for column in &columns {
            bytes = bytes.saturating_add(csv_cell_bytes(column));
        }
        for row in rows {
            bytes = bytes.saturating_add(columns.len().max(1));
            for column in &columns {
                let cell = match row.get(column) {
                    None | Some(serde_json::Value::Null) => std::borrow::Cow::Borrowed(""),
                    Some(serde_json::Value::String(value)) => {
                        std::borrow::Cow::Borrowed(value.as_str())
                    }
                    Some(value) => std::borrow::Cow::Owned(value.to_string()),
                };
                bytes = bytes.saturating_add(csv_cell_bytes(&cell));
                if bytes > MAX_CSV_BYTES {
                    return Some(ToolResult::error(format!(
                        "CSV export exceeds {MAX_CSV_BYTES} bytes"
                    )));
                }
            }
        }
        if bytes > MAX_CSV_BYTES {
            return Some(ToolResult::error(format!(
                "CSV export exceeds {MAX_CSV_BYTES} bytes"
            )));
        }
        None
    }
}

/// The high-risk flag blocks execution independently of the autonomy tier.
struct HighRiskCommands(Arc<SecurityPolicy>);

type ShellTool = GuardedTool<oh::tools::ShellTool, HighRiskCommands>;

impl ShellTool {
    fn new(
        security: Arc<SecurityPolicy>,
        runtime: Arc<dyn RuntimeAdapter>,
        audit: Arc<AuditLogger>,
    ) -> Self {
        Self {
            inner: oh::tools::ShellTool::new(Arc::clone(&security), runtime, audit),
            guard: HighRiskCommands(security),
        }
    }

    fn with_audit(
        self,
        audit: ShellAudit,
    ) -> GuardedTool<crate::harness::audit::AuditedShellTool, HighRiskCommands> {
        GuardedTool {
            inner: crate::harness::audit::AuditedShellTool::new(self.inner, audit),
            guard: self.guard,
        }
    }
}

impl ToolGuard for HighRiskCommands {
    fn timeout_policy(&self, inner: ToolTimeout) -> ToolTimeout {
        match inner {
            // The vocabulary moved from seconds to milliseconds at the 1ecf1b0
            // pin; the bound is the same hour.
            ToolTimeout::Millis(ms @ 1..=3_600_000) => ToolTimeout::Millis(ms),
            _ => ToolTimeout::Inherit,
        }
    }

    fn refusal(&self, args: &serde_json::Value) -> Option<ToolResult> {
        let command = args.get("command")?.as_str()?;
        if !self.0.block_high_risk_commands
            || self.0.command_risk_level(command) != oh::security::policy::CommandRiskLevel::High
            || self.0.check_gated_command(command).is_err()
        {
            return None;
        }
        Some(ToolResult::error(
            "[policy-blocked] Command blocked: high-risk commands are disallowed by policy",
        ))
    }
}

/// Subdirectory under the agent workspace that `curl` downloads land in.
const CURL_DEST_SUBDIR: &str = "downloads";

/// Every grant namespace a [`CapabilityFilter`] can gate — "which tool families
/// are budgeted".
///
/// This is the exec-grade surface [`namespace_of`] maps tools onto (`shell`,
/// `code`, `web`) plus the reserved `subagent` namespace. A capability plan's
/// budget map keys are validated against this set (a key outside it is a
/// manifest error), and it is the universe the fail-closed
/// [`capability_budget`](crate::harness::capability_budget) denies from when no
/// meter is available. Intrinsic tools (namespace `None`) are never listed here
/// — they are always kept regardless of the filter.
///
/// The canonical list lives in [`crate::company::GATEABLE_NAMESPACES`] (always
/// compiled, so manifest validation can see it in the default build); this is a
/// re-export for the harness call sites that key off it.
pub const GATEABLE_NAMESPACES: [&str; 7] = crate::company::GATEABLE_NAMESPACES;

/// Map a tool's runtime `name()` onto its grant namespace, or `None` when the
/// tool is **intrinsic** (memory / MCP / orchestrator / file / skill tools),
/// which are always kept regardless of the capability filter.
///
/// This is the single source of truth coupling a wired tool to the namespace
/// that gates it — [`filter_by_capabilities`] and any future capability-tier
/// logic key off it, so a new exec tool is added here and nowhere else.
pub fn namespace_of(tool_name: &str) -> Option<&'static str> {
    match tool_name {
        "shell" | "read_workspace_state" => Some("shell"),
        "apply_patch" | "git_operations" | "csv_export" => Some("code"),
        "web_fetch" | "http_request" | "curl" | "image_info" => Some("web"),
        // Media generation (issue #109). Mapped unconditionally — the arm is
        // inert without the `media` feature (the tools are never built), but the
        // namespace mapping is a pure string match so the capability filter and
        // the gateable-coverage invariant see `media` in every build.
        "media_generate_image" | "media_generate_video" | "media_list_models" => Some("media"),
        // Per-tenant Composio (issue #110). Mapped unconditionally for the same
        // reason as `media`: the arm is inert without the `composio` feature (the
        // tools are never built), but the namespace mapping is a pure string
        // match so the capability filter and the gateable-coverage invariant see
        // `composio` in every build.
        "composio_list_toolkits"
        | "composio_list_connections"
        | "composio_list_tools"
        | "composio_authorize"
        | "composio_execute" => Some("composio"),
        // Metered web search (issue #238). Lives in
        // [`search`](crate::harness::search) rather than this module because it
        // is a priced backend call, not an exec-grade local tool — but it is
        // namespaced here for the same reason `media` and `composio` are: the
        // capability filter and the gateable-coverage invariant must see
        // `search` in every build. Unlike those two the arm is never inert;
        // `web_search` compiles under the plain `openhuman` feature, which is
        // what CI actually builds and tests.
        "web_search" => Some("search"),
        // The same namespace under a company's OWN provider (the BYO half of
        // #238). `web_search` above is the canonical slot whichever provider
        // serves it — these are the provider extras that ride beside it. Mapped
        // here for the same reason: a company that budgets `search` must budget
        // every search tool, not only the metered one, or a capability ceiling
        // set on one provider evaporates when the operator switches to another.
        "exa_find_similar" | "exa_get_contents" | "brave_news_search" | "brave_image_search"
        | "brave_video_search" => Some("search"),
        _ => None,
    }
}

/// Whether `tool` is one of the raw HTTP web tools that take a plain `url`
/// argument — `web_fetch`, `http_request`, `curl`.
///
/// This is the `url`-taking subset of the `web` namespace: `image_info` is also
/// `web` but inspects a workspace file, so it is excluded. Kept here — beside
/// [`namespace_of`], the single source of truth for the family — so the S2
/// Composio deflection guardrail
/// ([`web_call_deflection`](crate::harness::composio_catalog::web_call_deflection),
/// consulted by
/// [`ApprovalPolicy::check`](crate::harness::policy::ApprovalPolicy)) recognises
/// the deflectable tools from one place rather than re-hardcoding the three
/// names where it hooks in.
pub fn is_web_request_tool(tool: &str) -> bool {
    matches!(tool, "web_fetch" | "http_request" | "curl")
}

/// Build the exec-grade [`SecurityPolicy`] shared by an agent's shell + code +
/// web tools, sandboxed to `workspace`.
///
/// Extends the same `workspace_only` shape [`file_tools`](crate::harness::build)
/// uses with the exec-relevant knobs:
///
/// * `autonomy` is mapped from the company [`PolicyMode`] — see
///   [`autonomy_for`], which is **not** 1:1 since `auto` (issue #560) has no
///   upstream counterpart, and which is a real security boundary on the
///   workflow `tool_call` path rather than a mapping detail.
/// * `block_high_risk_commands` is always on — destructive shell commands are
///   refused before they spawn.
/// * `require_approval_for_medium_risk` covers Supervised **and** Auto, for the
///   reason argued on [`autonomy_for`]: the flag is inert unless `autonomy` is
///   `Supervised`, so leaving `auto` out of it would silently undo the very
///   mapping chosen to keep `auto` from loosening shell execution.
/// * `allow_tool_install` and `auto_approve_all` are always off — an embedded
///   company agent never installs OS packages nor blanket-approves itself.
///
/// This policy is *advisory-strict*: opencompany's own
/// [`ApprovalPolicy`](crate::harness::policy::ApprovalPolicy) tool policy is the
/// authoritative park/deny gate layered above it (unlike the MCP bridge, which
/// passes a permissive OpenHuman policy).
pub fn exec_security(workspace: &Path, mode: PolicyMode) -> SecurityPolicy {
    let dir = workspace.to_path_buf();
    SecurityPolicy {
        autonomy: autonomy_for(mode),
        workspace_dir: dir.clone(),
        action_dir: dir,
        workspace_only: true,
        block_high_risk_commands: true,
        require_approval_for_medium_risk: matches!(mode, PolicyMode::Supervised | PolicyMode::Auto),
        allow_tool_install: false,
        auto_approve_all: false,
        ..SecurityPolicy::default()
    }
}

/// The OpenHuman [`AutonomyLevel`] a company [`PolicyMode`] maps to.
///
/// Three of the four map by name. `auto` (issue #560) has no upstream
/// counterpart — OpenHuman's `AutonomyLevel` is `ReadOnly` / `Supervised` /
/// `Full` — so it must borrow one, and **the borrowed level is a security
/// decision, not a naming one.**
///
/// # Why `Supervised` and not `Full`
///
/// `auto` is more permissive than `supervised` at opencompany's own
/// [`ApprovalPolicy`](crate::harness::policy::ApprovalPolicy), which is where
/// the tier is supposed to be expressed. Reaching for the matching *feel* here
/// and mapping it to `Full` would loosen a different gate in the opposite
/// direction, because this policy is not always sitting underneath that one:
///
/// * A workflow `tool_call` node runs through
///   [`WorkflowToolInvoker`](crate::workflows::caps), which gates on the
///   `[tools].allow` grant list and **this policy** — no `ApprovalPolicy` is
///   installed on that path. (Workflow *agent* nodes go through the roster and
///   do have one.) So for those nodes this is the whole tier.
/// * Upstream, `AutonomyLevel::Full` stops asking about medium-risk shell
///   commands: the approval arm in `command_checks` fires only when `autonomy
///   == Supervised`. Mapping `auto` to `Full` would therefore let medium-risk
///   commands run unapproved in workflow nodes on an `auto` company — while the
///   tier's stated contract is that `shell` parks. The inverse of what the
///   operator selected.
///
/// Mapping to `Supervised` costs nothing in the other direction. Every tool
/// this policy governs — `shell`, the code runners, the web tools — is
/// `Standing::PerCall` and therefore parks under `auto` at the authoritative
/// layer anyway, so the stricter advisory tier underneath is never the thing
/// the operator notices. `auto` buys its autonomy in tools this policy does not
/// govern.
///
/// This is also why `require_approval_for_medium_risk` in [`exec_security`]
/// lists `Auto` explicitly instead of leaving `mode == PolicyMode::Supervised`
/// to answer it. That expression was exhaustive by accident and would have
/// quietly returned `false` for the new variant — pairing `Supervised` autonomy
/// with the medium-risk gate switched off, which is the loosening this mapping
/// was chosen to prevent, arriving through the back door.
fn autonomy_for(mode: PolicyMode) -> AutonomyLevel {
    match mode {
        PolicyMode::Readonly => AutonomyLevel::ReadOnly,
        PolicyMode::Supervised | PolicyMode::Auto => AutonomyLevel::Supervised,
        PolicyMode::Full => AutonomyLevel::Full,
    }
}

/// A native (host-process) [`RuntimeAdapter`] for the shell tool. Stateless and
/// cheap — a fresh handle per agent keeps tenants from sharing runtime state.
pub fn native_runtime() -> Arc<dyn RuntimeAdapter> {
    Arc::new(NativeRuntime::new())
}

/// A shell audit logger paired with the file it appends to.
///
/// The pairing is structural on purpose. [`AuditLogger`] does not expose its own
/// path, and the fail-closed refusal in
/// [`AuditedShellTool`](crate::harness::audit::AuditedShellTool) has to *name*
/// the sink it could not write — an operator staring at a shell outage needs
/// that path. Carrying the two together means the name can never describe a
/// different file than the one being appended to.
#[derive(Clone)]
pub struct ShellAudit {
    /// The shared per-agent logger. Cloning shares one instance, so every
    /// append serializes through its write lock.
    pub logger: Arc<AuditLogger>,
    /// The file `logger` appends to, derived from the same
    /// [`AuditConfig::default`] the logger was built with.
    pub sink: std::path::PathBuf,
}

impl ShellAudit {
    /// A disabled logger over a sentinel `sink`, for tests and contexts that
    /// need a handle but must not touch the filesystem. `log()` short-circuits
    /// before any I/O, so the sink path is never opened.
    pub fn disabled() -> Self {
        Self {
            logger: AuditLogger::disabled(),
            sink: std::path::PathBuf::new(),
        }
    }
}

impl std::fmt::Debug for ShellAudit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellAudit")
            .field("sink", &self.sink)
            .finish_non_exhaustive()
    }
}

/// The per-agent [`AuditLogger`] for shell command execution, built the way
/// OpenHuman's `runtime_node::build_runtime_tools` does, but keyed on a
/// **host-owned** sink directory rather than the agent's workspace.
///
/// `audit_dir` is
/// [`DataLayout::agent_audit_dir`](crate::store::DataLayout::agent_audit_dir) —
/// `companies/<slug>/audit/<agent>/`, per agent and outside every agent
/// workspace. Two properties depend on that, and neither survives putting the
/// sink back in the workspace:
///
/// * The workspace is also the `workspace_only` [`SecurityPolicy`] root the
///   file tools enforce, so a sink inside it is a **policy-permitted** target:
///   the plain relative `file_write("audit.log")` — no traversal, no absolute
///   path, no `shell` — is exactly what the policy allows, and it used to land
///   on the audit trail. Outside the workspace that same call reaches nothing
///   that matters (issue #775). Absolute paths and `../` are refused either
///   way, by `workspace_only`'s own rules; they are not what moved.
/// * The vendored registry caches one logger per *directory*, first config
///   wins, so a directory shared between agents would hand the second agent the
///   first agent's file.
///
/// The directory is created here, before the logger is built: the vendored
/// factory keys its process-global registry on the *canonicalized* path and
/// silently falls back to the raw one when the directory is missing, which
/// registers one physical sink twice and reopens the interleaving race the
/// registry exists to prevent.
///
/// Returns `None` when the directory cannot be created or the logger cannot be
/// initialized. Callers **must** treat `None` as fail-closed: [`shell_tools`]
/// withholds the `ShellTool` entirely rather than register it unaudited (see
/// there). The failure is logged at `error!` (not `warn!`): losing the audit
/// logger drops shell capability for the agent, so the event must surface in
/// production error telemetry.
///
/// This makes the sink unreachable *through the sanctioned tool paths*. It is
/// not tamper-evidence — the shell runs as the same uid and can still delete the
/// file. See `docs/spec/security/agent-isolation.md`.
pub fn shell_audit(audit_dir: &Path) -> Option<ShellAudit> {
    if let Err(error) = std::fs::create_dir_all(audit_dir) {
        tracing::error!(
            audit_dir = %audit_dir.display(),
            %error,
            "[toolbelt] shell audit sink directory could not be created; withholding shell capability (fail-closed) — this agent gets NO shell tool"
        );
        return None;
    }
    let config = AuditConfig::default();
    let sink = audit_dir.join(&config.log_path);
    match get_or_create_workspace_audit_logger(config, audit_dir.to_path_buf()) {
        Ok(logger) => Some(ShellAudit { logger, sink }),
        Err(error) => {
            tracing::error!(
                audit_dir = %audit_dir.display(),
                %error,
                "[toolbelt] shell audit logger init failed; withholding shell capability (fail-closed) — this agent gets NO shell tool"
            );
            None
        }
    }
}

/// The `shell` namespace tools, sharing the exec-grade `security` policy and
/// pinned to the agent's `workspace`. Mirrors
/// [`file_tools`](crate::harness::build): a flat vector the builder extends.
///
/// Kept **disjoint** from [`code_tools`] so the builder can gate each grant
/// namespace independently — a company granting only `code` must never receive
/// a live [`ShellTool`], and vice versa (the production [`CapabilityFilter`] is
/// `AllowAll`/identity and does not re-trim namespaces post-construction).
///
/// * `shell` — run commands (needs `runtime` + `audit`; plain constructor, no
///   Node/Python bootstrap in v1).
/// * `read_workspace_state` — read-only git/tree overview.
///
/// **Fail closed on audit:** `audit` is `None` only when the per-agent audit
/// logger could not be initialized (see [`shell_audit`]). In that case the whole
/// `shell` namespace is withheld — an empty vector — so a `ShellTool` can never
/// run commands with no audit record. Dropping the capability is the safe
/// failure mode; registering an unaudited shell is not.
///
/// **Fail closed at run time too:** the `ShellTool` is wrapped in an
/// [`AuditedShellTool`](crate::harness::audit::AuditedShellTool), which appends
/// the command's intent line *before* delegating and refuses the call when that
/// append fails. Init-time fail-closed alone was not enough — upstream's
/// post-execution `emit_audit` is warn-and-continue by design, so a sink that
/// became unwritable *after* the agent was built would let commands run with no
/// record at all.
pub fn shell_tools(
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    audit: Option<ShellAudit>,
    workspace: &Path,
) -> Vec<Box<dyn Tool>> {
    let Some(audit) = audit else {
        return Vec::new();
    };
    vec![
        Box::new(ShellTool::new(security, runtime, Arc::clone(&audit.logger)).with_audit(audit)),
        Box::new(WorkspaceStateTool::new(workspace.to_path_buf())),
    ]
}

/// The sandbox brief: what the agent's own working directory is, which tools
/// reach it, and the path confinement the file/code tools enforce there.
///
/// # Why this exists
///
/// Every other granted surface on this belt names itself in the prompt —
/// [`workspace_brief`](crate::harness::workspace_tools::workspace_brief) for the
/// shared note tree, [`ledger_brief`](crate::harness::ledger_tools::ledger_brief)
/// for the company's own records,
/// [`publish_brief`](crate::harness::publish::publish_brief) for handing a file
/// over. The agent's **sandbox** did not. `publish_brief` mentioned it in
/// passing, as the place a deliverable comes from, and only when an artifact
/// store happened to be wired; it named no tool, and it said nothing at all
/// about `shell`.
///
/// The observed failure is the one issue #237's brief exists to prevent, one
/// surface over: asked to *write* something, an agent that has never been told
/// it holds `file_write` records a task about writing it instead. It is not
/// refusing — it is picking the surface it was told about. A granted tool that
/// goes unmentioned is, for prompt purposes, a tool that was never granted, and
/// the `shell` namespace was in exactly that state: wired since Cell A, named
/// nowhere.
///
/// # Why it is assembled from flags rather than written once
///
/// `files`, `shell` and `code` are three independent grant namespaces
/// ([`build_agent`](crate::harness::build::build_agent) gates each separately),
/// so a single fixed paragraph would describe tools some agents do not hold —
/// the precise mistake `publish_brief`'s own comment warns about, and one that
/// costs a turn per hallucinated call. Each clause is therefore emitted only
/// under the flag that wired its tools, and an agent holding none of the three
/// gets the empty string and no section at all.
///
/// The confinement sentence is not decoration, and it is scoped on purpose.
/// [`exec_security`] sets `workspace_only`, so the **file** and `code` tools
/// refuse an absolute path or a `../` escape by policy rather than by the
/// model's judgement; an agent that does not know this spends its turns
/// discovering it one refusal at a time. The **shell** is deliberately not
/// described that way: `action_dir` only sets the command's current directory,
/// and a same-uid command can read anywhere the server can
/// (`docs/spec/security/agent-isolation.md`). The shell clause says the
/// directory is where commands *start*, never that they cannot leave it.
pub fn sandbox_brief(files: bool, shell: bool, code: bool) -> String {
    if !files && !shell && !code {
        return String::new();
    }
    let mut brief = String::from(
        "\n\n## Your sandbox\n\
         You have a real working directory of your own — a private folder on disk, separate from \
         the company workspace (the shared note tree the `workspace_*` tools read). It is where \
         your own files live.\n\
         Every path you give these tools is relative to that directory, so write `report.md` or \
         `drafts/report.md`, never `/tmp/report.md` or `~/report.md`.\n",
    );
    if files {
        brief.push_str(
            "Read and write it with `file_read`, `file_write`, `edit`, `list`, `glob` and \
             `grep`. Subdirectories are created for you on write, and an absolute path or a \
             `../` escape is refused by these tools. A file you write or edit this way also \
             lands in the company workspace under your own `agents/` folder, so your reply can \
             point at it and anyone can open it.\n",
        );
    }
    if shell {
        brief.push_str(
            "Run commands with `shell`. It starts in that same directory, so write a command \
             against relative paths like any other tool call. `read_workspace_state` gives you \
             a read-only overview of what is there. Every command is recorded to an audit log \
             the operator can read.\n",
        );
    }
    if code {
        brief.push_str(
            "`apply_patch` applies a structured multi-file edit, `git_operations` runs \
             status/diff/log/commit inside the directory, and `csv_export` writes a CSV into \
             `exports/`.\n",
        );
    }
    if shell {
        brief.push_str(
            "When you are asked to write, produce, build or run something, do it here — \
             actually write the file or run the command.",
        );
    } else {
        brief.push_str(
            "When you are asked to write, produce or build something, do it here — actually \
             write the file.",
        );
    }
    brief.push_str(
        " Recording a task about the work, or pasting \
         the finished text into your reply, is not the same as producing it, and leaves nothing \
         on disk for anyone to open.\n\
         A command or write that touches something consequential may be held for operator \
         approval before it runs. That is a pause, not a failure: you will be told the outcome. \
         Until you are, do not report the work as done.",
    );
    brief
}

/// Describe the live public-web surface without promising a search backend the
/// deployment did not wire.
///
/// URL fetch and URL discovery are deliberately separate grants/backends. That
/// distinction must be visible to the model: otherwise a research agent that
/// lacks `web_search` repeatedly searches the company workspace, even though it
/// can still verify known official URLs with `web_fetch`.
pub fn web_brief(fetch: bool, search: bool) -> String {
    if !fetch && !search {
        return String::new();
    }

    let mut brief = String::from("\n\n## Public web\n");
    if fetch {
        brief.push_str(
            "Use `web_fetch` to read and cite a public URL you already know. Use `http_request` \
             for an API or a non-GET request, and `curl` only when you need to download a file \
             into your sandbox. These fetch tools do not discover URLs.\n",
        );
    }
    if search {
        if fetch {
            brief.push_str(
                "Use `web_search` to discover current sources, then open the strongest results \
                 with `web_fetch` before making claims.\n",
            );
        } else {
            brief.push_str(
                "Use `web_search` to discover current sources. URL fetching is not granted for \
                 this turn, so ground claims in the search results and do not invent page \
                 contents you could not open.\n",
            );
        }
        brief.push_str(
            "If `web_search` reports an authentication, expired-session, missing-credential, or \
             unavailable-provider error, stop after that one call and report the exact blocker. \
             A different query cannot repair credentials, so do not retry it or substitute local \
             workspace reads for the missing public sources.\n",
        );
    } else {
        brief.push_str(
            "No `web_search` provider is connected for this turn. For research, verify official \
             URLs you know with `web_fetch`; if discovery is essential, say specifically that a \
             Search provider must be connected. Do not substitute repeated workspace or ledger \
             reads for public-web discovery.\n",
        );
    }
    brief
}

/// The `code` namespace tools, sharing the exec-grade `security` policy and
/// pinned to the agent's `workspace`. Disjoint from [`shell_tools`] (see there
/// for why the split is a security boundary, not just cosmetics). Unlike shell,
/// these need neither a host runtime nor an audit logger.
///
/// * `apply_patch` — structured multi-edit patches.
/// * `git_operations` — status/diff/log/commit within the workspace.
/// * `csv_export` — write a CSV into the workspace's `exports/` dir.
pub fn code_tools(security: Arc<SecurityPolicy>, workspace: &Path) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ApplyPatchTool::new(security.clone())),
        Box::new(GitOperationsTool::new(
            security.clone(),
            workspace.to_path_buf(),
        )),
        Box::new(CsvExportTool::new(security)),
    ]
}

/// The `web` tools, sharing the exec-grade `security` policy and the
/// per-company `allowed_domains` SSRF allowlist.
///
/// `allowed_domains` semantics (OpenHuman's `url_guard`, applied inside each
/// constructor): an **empty** list is *open mode* — all public hosts allowed —
/// while private/loopback/link-local/multicast/metadata IPs are **always**
/// rejected regardless. A non-empty list is strict (only those hosts +
/// subdomains); `"*"` is an explicit allow-all-public wildcard.
///
/// * `web_fetch` — fetch + return page text (size/timeout defaults).
/// * `http_request` — arbitrary HTTP method/headers/body.
/// * `curl` — download a URL into the workspace `downloads/` dir.
/// * `image_info` — inspect a workspace image's dimensions/format.
pub fn web_tools(
    security: Arc<SecurityPolicy>,
    allowed_domains: Vec<String>,
    workspace: &Path,
) -> Vec<Box<dyn Tool>> {
    // Source size/timeout defaults from OpenHuman's own config so there is one
    // source of truth (and no `0 → coerced-with-warning` noise on each build).
    let http_defaults = HttpRequestConfig::default();
    vec![
        Box::new(WebFetchTool::new(
            security.clone(),
            allowed_domains.clone(),
            None,
            None,
        )),
        Box::new(HttpRequestTool::new(
            security.clone(),
            allowed_domains.clone(),
            http_defaults.max_response_size,
            http_defaults.timeout_secs,
        )),
        Box::new(CurlTool::new(
            security.clone(),
            allowed_domains,
            workspace.to_path_buf(),
            CURL_DEST_SUBDIR.to_string(),
            http_defaults.max_response_size as u64,
            http_defaults.timeout_secs,
        )),
        Box::new(ImageInfoTool::new(security)),
    ]
}

/// The managed platform credential for the media-generation backend (issue
/// #109) — the OpenHuman backend URL + the platform's own bearer token.
///
/// **Security invariant**: this holds ONLY the managed platform credential,
/// never a tenant BYOK key. The tenant identity that the backend bills is
/// derived server-side from this managed credential, so a company can never
/// point media generation at a key it controls. Threaded onto
/// [`HarnessDeps`](crate::harness::HarnessDeps) and consumed by [`media_tools`].
///
/// Always compiled (so the deps field exists in every `openhuman` build and
/// every construction site fails closed with `None`); the live tool
/// constructors in [`media_tools`] are gated behind the `media` feature.
#[derive(Clone)]
pub struct MediaBackend {
    /// The media-generation backend base URL (e.g. `https://api.tinyhumans.ai`).
    pub backend_url: String,
    /// The managed platform bearer credential. Never a tenant key.
    pub auth_token: String,
}

impl std::fmt::Debug for MediaBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never let the managed credential land in a trace.
        f.debug_struct("MediaBackend")
            .field("backend_url", &self.backend_url)
            .field(
                "auth_token",
                &if self.auth_token.is_empty() {
                    "<unset>"
                } else {
                    "<redacted>"
                },
            )
            .finish()
    }
}

impl MediaBackend {
    /// Whether [`backend_url`](Self::backend_url) is exactly `https` — the
    /// gate [`media_tools`] enforces before it will spend the managed
    /// credential. Evidence-gathering reuses this so a company can never be
    /// told `media` is natively wired when the wiring path would refuse the
    /// backend.
    pub fn is_https(&self) -> bool {
        url::Url::parse(&self.backend_url)
            .map(|parsed| parsed.scheme() == "https")
            .unwrap_or(false)
    }
}

/// The `media` namespace tools (issue #109): image + video generation plus the
/// model catalog, built over the MANAGED platform credential in `backend` with
/// generated artifacts pinned to the agent's `workspace` (the tools' persistence
/// `action_dir`, so nothing escapes the sandbox).
///
/// These spend real money — the backend charges on submit — so
/// [`build_agent`](crate::harness::build::build_agent) only calls this when the
/// company **explicitly** grants `media` (never via the `*` wildcard) AND a
/// managed credential is present; the generate tools additionally park for
/// operator approval through the [`ApprovalPolicy`](crate::harness::policy).
///
/// * `media_generate_image` / `media_generate_video` — submit → poll → persist
///   (each saved file also filed as a workspace artifact), billed by the
///   backend.
/// * `media_list_models` — read-only catalog GET (needs no `action_dir`).
///
/// Gated on the `media` feature; enabling it necessarily enables
/// `openhuman_core/media`, so the upstream tool types are in scope.
#[cfg(feature = "media")]
pub fn media_tools(backend: &MediaBackend, workspace: &Path) -> Vec<Box<dyn Tool>> {
    use oh::media::generation::{MediaGenerators, OPENROUTER_PROXY_PATH, media_tools_from};
    use tinyagents_harness::tinyinference_image::{
        MediaAuth, MediaTransport, OpenRouterImageGenerator,
    };
    use tinyagents_harness::tinyinference_video::{OpenRouterVideoGenerator, WaitPolicy};

    // Fail closed on any backend that is not exactly HTTPS: the transport
    // attaches the managed platform token and the backend charges real money
    // on submit, so an `http://` override would ship the credential over the
    // wire. The default (`https://api.tinyhumans.ai`) passes; a misconfigured
    // host gets no media tools at all, loudly, rather than a client that leaks.
    if !backend.is_https() {
        tracing::warn!(
            backend_url = %backend.backend_url,
            "[toolbelt] refusing to wire the media tools: the backend URL must be https"
        );
        return Vec::new();
    }

    // The Config-free seam. OpenHuman's own `build_media_tools` resolves the
    // endpoint and credential from a global `Config`; this host has neither,
    // so it builds the same OpenRouter generators over the backend's
    // `/agent-integrations/openrouter` proxy with the managed credential it
    // was handed, and lets upstream's `media_tools_from` bind them under the
    // pinned tool names (`media_generate_image` / `media_generate_video` /
    // `media_list_models`) the approval policy parks on.
    crate::harness::backend_transport::ensure_installed();
    let http = match oh::util::tls::tls_client_builder()
        .default_headers(openhuman_tinyhumans::backend::product_identity_headers())
        .build()
    {
        Ok(http) => http,
        Err(error) => {
            tracing::warn!(%error, "[toolbelt] media tools skipped: HTTP client build failed");
            return Vec::new();
        }
    };
    let base = openhuman_core::util::url::join_url(&backend.backend_url, OPENROUTER_PROXY_PATH);
    let transport = MediaTransport::new(MediaAuth::ApiKey(backend.auth_token.clone()))
        .with_client(http)
        .with_base_url(&base);
    let generators = MediaGenerators {
        image: Arc::new(OpenRouterImageGenerator::with_transport(transport.clone())),
        video: Arc::new(OpenRouterVideoGenerator::with_transport(transport)),
    };
    media_tools_from(
        generators,
        workspace,
        workspace,
        WaitPolicy::new(
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(600),
        ),
    )
}

/// The `subagent` namespace — **reserved and empty in v1**.
///
/// OpenHuman's sub-agent spawn tools (`SpawnSubagentTool` et al.) reach a
/// process-global agent registry and can bypass per-agent budget accounting —
/// both unsafe under opencompany's multi-tenant isolation model. The namespace
/// is reserved so a company can grant `subagent` today without effect, and a
/// future cell can wire a tenant-safe delegation surface without a grant change.
pub fn subagent_tools() -> Vec<Box<dyn Tool>> {
    Vec::new()
}

/// Which tools an agent may keep after its grants have already admitted them —
/// the seam the capability-tier gate ([`capability_budget`](crate::harness::capability_budget))
/// constructs per tenant, per turn.
///
/// [`AllowAll`](Self::AllowAll) is the identity pass (no plan configured, or a
/// tenant under every tier's budget); [`DenyNamespaces`](Self::DenyNamespaces)
/// drops the exec families whose per-period token budget the tenant has spent
/// through — or, fail-closed, every gateable family when the meter can't be
/// read.
#[derive(Clone, Debug, Default)]
pub enum CapabilityFilter {
    /// Keep every tool the grants admitted (identity).
    #[default]
    AllowAll,
    /// Drop every tool whose [`namespace_of`] is in this set; intrinsic tools
    /// (namespace `None`) are always kept.
    DenyNamespaces(std::collections::HashSet<&'static str>),
}

/// Apply a [`CapabilityFilter`] to a built tool vector, just before it is handed
/// to the [`AgentBuilder`](openhuman_core::agent::AgentBuilder).
///
/// Intrinsic tools (memory / MCP / orchestrator / file / skill — any tool whose
/// [`namespace_of`] is `None`) are always kept; only namespaced exec tools can
/// be dropped. [`CapabilityFilter::AllowAll`] is the identity pass.
pub fn filter_by_capabilities(
    tools: Vec<Box<dyn Tool>>,
    filter: &CapabilityFilter,
) -> Vec<Box<dyn Tool>> {
    match filter {
        CapabilityFilter::AllowAll => tools,
        CapabilityFilter::DenyNamespaces(denied) => tools
            .into_iter()
            .filter(|tool| match namespace_of(tool.name()) {
                Some(namespace) => !denied.contains(namespace),
                // Intrinsic tools have no namespace and are never dropped.
                None => true,
            })
            .collect(),
    }
}

/// The native capability namespaces actually present on a built tool belt.
///
/// Derived the same way [`filter_by_capabilities`] reads the belt — each tool's
/// [`namespace_of`] — kept to the shared native vocabulary
/// ([`native_capability_namespaces`](crate::company::native_capability_namespaces)),
/// so a tool that was wired makes its namespace show up here and one that was
/// not never does. Sorted and unique for a stable system-prompt rendering.
pub fn native_capabilities_on_belt(
    tools: &[Box<dyn Tool>],
) -> std::collections::BTreeSet<&'static str> {
    let native = crate::company::native_capability_namespaces();
    tools
        .iter()
        .filter_map(|tool| namespace_of(tool.name()))
        .filter(|ns| native.contains(ns))
        .collect()
}

/// The native capability namespaces the composio brief may credit this agent
/// with holding — [`native_capabilities_on_belt`] narrowed by the SAME
/// per-turn [`CapabilityFilter`] [`filter_by_capabilities`] is about to apply
/// to the belt itself.
///
/// `build_agent` computes `native_caps` for the composio brief from the
/// PRE-filter `tools` vector (`filter_by_capabilities` does not run until
/// after every brief, including this one, is rendered) — mirroring it here
/// against [`namespace_denied`] is what keeps the brief from crediting a
/// namespace `filter_by_capabilities` is about to strip from the live belt.
/// Before this existed, only the Composio side of that same brief carried the
/// check (`composio_capability_admits`); the native side was missed, so a
/// tier denying e.g. `search` while admitting `composio` still told the
/// agent it held a built-in search tool it did not.
pub fn native_caps_for_composio_brief<'a>(
    tools: &'a [Box<dyn Tool>],
    filter: &CapabilityFilter,
) -> Vec<&'a str> {
    native_capabilities_on_belt(tools)
        .into_iter()
        .filter(|namespace| !namespace_denied(filter, namespace))
        .collect()
}

/// Whether a [`CapabilityFilter`] denies a given namespace — the same test
/// [`filter_by_capabilities`] applies per tool, exposed standalone so a
/// caller that needs the outcome without a tool vector in hand (the sandbox
/// brief, built before tools are filtered) can ask it directly.
///
/// [`CapabilityFilter::AllowAll`] denies nothing; a name outside
/// [`GATEABLE_NAMESPACES`] cannot be denied by construction (`DenyNamespaces`
/// is only ever populated from that set), so this returns `false` for those
/// too rather than requiring the caller to special-case them.
pub fn namespace_denied(filter: &CapabilityFilter, namespace: &str) -> bool {
    match filter {
        CapabilityFilter::AllowAll => false,
        CapabilityFilter::DenyNamespaces(denied) => denied.contains(namespace),
    }
}

/// Whether the per-tenant Composio surface may be wired for this agent's
/// current turn — one predicate shared by both the S1 brief
/// ([`build::build_agent`](crate::harness::built_in::build::build_agent)) and
/// the S2 deflection policy
/// ([`build_roster`](crate::harness::built_in::build_roster)) so the two
/// cannot drift back out of lockstep the way they did before this fix (PR
/// #1780 review, issue #1759).
///
/// `wired` is the grant+credential outcome each call site already resolved
/// (an explicit `composio` grant AND a resolved credential with a non-empty
/// toolkit allowlist) — this function does not re-derive it, only narrows it
/// by the per-turn capability tier. When [`namespace_denied`] reports
/// `composio` denied (a `free`/`starter`/`pro` plan's Composio budget is
/// exhausted, or a fail-closed metering error), `filter_by_capabilities`
/// strips every `composio_*` tool from the belt — describing the brief or
/// installing the deflection anyway would ground the agent in a surface it no
/// longer holds, or point a blocked web call at a tool that is not on the
/// belt.
///
/// Deliberately NOT behind the `composio` feature, like the rest of this
/// module's namespace plumbing and like `composio_catalog`'s own S1/S2 pair —
/// pure logic stays outside the gate so CI's fast, always-run `openhuman`
/// lane exercises it, rather than only the `composio` feature's partial lane.
pub fn composio_capability_admits(wired: bool, capabilities: &CapabilityFilter) -> bool {
    wired && !namespace_denied(capabilities, "composio")
}

#[cfg(test)]
#[path = "toolbelt_media_filter_tests.rs"]
mod tests_media_filter;
#[cfg(test)]
#[path = "toolbelt_shape_tests.rs"]
mod tests_shape;
#[cfg(test)]
#[path = "toolbelt_shell_security_tests.rs"]
mod tests_shell_security;
#[cfg(test)]
#[path = "toolbelt_workspace_io_tests.rs"]
mod tests_workspace_io;
#[cfg(test)]
#[path = "toolbelt_test_helpers_tests.rs"]
mod toolbelt_test_helpers_tests;
