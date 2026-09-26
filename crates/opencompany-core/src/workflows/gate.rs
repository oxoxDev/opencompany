//! Issues #460 and #614: the company's [`ApprovalPolicy`] decides which nodes
//! stop for an operator, before the run reaches them.
//!
//! Two node kinds reach an outside-world capability with no turn behind them,
//! and both are gated here so they answer to one declaration: `tool_call`
//! (issue #460) and `http_request` (issue #614). See [`call_of`].
//!
//! # The hole this closes
//!
//! [`WorkflowToolInvoker::invoke`](super::caps) resolves a slug's grant
//! namespace, checks it fail-closed against `[tools].allow`, looks the tool up
//! and executes it. [`ApprovalPolicy`] is never consulted, so a `tool_call`
//! node running `shell` or `http_request` on a `supervised` company produced no
//! approval card, no recorded decision and no grant accounting — while the
//! *same call* from an agent node in the *same graph* did, ever since #395.
//!
//! The `http_request` node kind had the identical hole on a different seam —
//! [`GuardedHttpClient`](super::caps) calls `tool.execute(args)` directly, never
//! `ToolInvoker` — so #460's fix did not reach it. What still held there, and
//! still does: the SSRF `url_guard` on every request and redirect, the
//! company's `web_allowed_domains` allowlist, the size/timeout caps, and
//! `readonly` blocking the call outright via `can_act()`. What was missing is
//! the same operator-facing half. Worth stating precisely, because "no approval
//! card under `supervised`" and "ungated arbitrary HTTP" are very different
//! claims and only the first one is true.
//!
//! What was NOT missing is exec-security: `[policy].mode` has always fed
//! [`toolbelt::exec_security`](crate::harness::toolbelt), which sets the
//! autonomy tier, blocks high-risk commands and confines execution to the
//! workflow workspace. That layer is untouched here. The gap was the
//! operator-facing half, and this closes only that.
//!
//! # Why the gate is not in the invoker
//!
//! The obvious shape — consult the policy inside `invoke` and refuse — is
//! wrong twice over, and both reasons are load-bearing.
//!
//! **It would be a regression, not a fix.** An agent node parks *after* a turn
//! that already happened: openhuman resolves the refusal inline, the model is
//! told no and narrates it, and the node still succeeds. A `tool_call` node has
//! no turn — the call **is** the node — so a refusal fails the node, and
//! `on_error` defaults to `stop`. Under the default `supervised` mode
//! [`consequence_of`](crate::policy::consequence_of) puts `shell`,
//! `http_request`, `curl`, `web_fetch`, `apply_patch`, `csv_export` and
//! `git_operations` at [`Reach::Consequence`](crate::policy::Reach), so
//! refusing in the invoker would break essentially every effectful `tool_call`
//! node on every default-mode company.
//!
//! **The card could not be honoured.** Approving a harness-projected tool call
//! mints a single-use [`GrantedCall`](crate::runtime::grants::GrantedCall) that
//! is redeemed by re-dispatching *the agent that asked*. There is no agent on
//! this path, so nothing would ever redeem it: the grant would expire on its
//! TTL and the operator would be told the agent did not act, about a call no
//! agent ever made.
//!
//! That argument is about the **single-use** grant and only it, which is why
//! issue #1098 could open the standing arm without touching this one. A
//! [`StandingGrant`](crate::runtime::grants::StandingGrant) is never redeemed by
//! anybody: it is matched at the gate, before the call, and the call then
//! proceeds in place. Nothing has to be re-dispatched, so "nothing would ever
//! redeem it" is not a property of it. `consume_grant` therefore stays closed on
//! this path permanently and
//! [`standing_grant_allows`](crate::harness::policy::ApprovalPolicy) does not —
//! the asymmetry is the safety argument, not an oversight to tidy up.
//!
//! And the seam says so itself. [`ToolInvoker::invoke`](tinyflows::caps::ToolInvoker)
//! receives `(slug, args, conn)` — no node id, no run state. It cannot name the
//! node on a card, and on a continuation it could not recognise a call the
//! operator had already approved. A fix that has to invent both does not belong
//! there.
//!
//! # Where it goes instead
//!
//! One level up, where the host holds both facts. [`run_workflow_inner`] already
//! translates the graph **per run**, and holds the [`CompanyRecord`] (the mode
//! and the grants) — so the policy verdict is taken there and written onto the
//! node as the engine's own `requires_approval` flag.
//!
//! That flag is a **generic per-node** gate in tinyflows, not a node kind: the
//! engine checks it before the node's work and, once the node's id is listed in
//! the run input's `approvals` array, falls through and executes the node
//! normally. So a marked `tool_call` node inherits every piece #395 already
//! built, unchanged:
//!
//! * the engine pauses before the tool runs, and reports the node on
//!   `pending_approvals`;
//! * [`park_pending_gates`](super::runner) turns that into a decidable card;
//! * [`resume_from_effect`](crate::runtime::workflow_resume) re-runs the graph
//!   with the approval in the trigger input, and the call executes;
//! * the #438 delivery ledger stops the continuation re-sending reports.
//!
//! It also means the gate is visible **on the canvas**, which is the other half
//! of what #460 complains about: two node kinds in one graph, governed
//! differently, with nothing saying which is which.
//!
//! # This is not #561
//!
//! #460's acceptance criteria ask for suspend/resume, and the issue's Related
//! section points at #561 as the expensive way to get it. That reading is
//! wrong, and the correction matters for anyone scoping the neighbouring
//! issues: #561 is expensive because the fail-closed block lives in
//! **openhuman's turn loop**, and approving therefore costs a re-dispatch. The
//! workflow path never enters that loop. A paused tinyflows run is *settled*,
//! and #395 already shipped resume-by-re-run for exactly this shape.
//!
//! # Deliberate deviations, stated rather than quietly satisfied
//!
//! * **Standing grants now apply on this path too** (issue #1098). #460 left
//!   this fail-closed and named the reason: a standing grant was scoped to a
//!   teammate, a `tool_call` node has none, and *"widening that is a consent
//!   decision for the maintainer, not one to make by defaulting."* #1098 is that
//!   decision taken. The workflow is now a real grant subject
//!   ([`GrantSubject::Workflow`](crate::runtime::grants::GrantSubject)), so a
//!   permission the operator gave *this workflow* opens its gates until the
//!   deadline — which is what stops a scheduled run re-asking the same question
//!   every time it fires.
//!
//!   Three things did **not** widen with it. A *teammate's* grant still does not
//!   open a workflow node's gate: the two subjects are separate namespaces and a
//!   workflow named like a teammate matches nothing of theirs. The permission is
//!   still refused unless the call is grantable on its own arguments, so
//!   `shell` and `http_request` keep asking every run. And the **single-use**
//!   arm is untouched — see the note below.
//! * **A `Deny` verdict is not enforced here.** Under `readonly`,
//!   [`ApprovalPolicy::check`] denies external effects outright. Honouring that
//!   would be a *new refusal* on a path that runs today, i.e. exactly the
//!   regression argued against above, and #460 is explicit that exec-security
//!   (which already applies the `readonly` autonomy tier) is not the gap. Only
//!   `RequireApproval` gates.
//! * **An authored `requires_approval = false` does not win.** The policy adds
//!   a gate the author did not ask for; an author cannot opt their node out of
//!   the company's approval policy.
//! * **`sub_workflow` children are gated by their resolver.** The child is
//!   translated inside tinyflows after this pass has handled the top-level
//!   graph, so [`StoreWorkflowResolver`](super::caps::resolver::StoreWorkflowResolver)
//!   applies the same policy and grants before returning it. tinyflows surfaces
//!   the pause to the parent with a namespaced node id and forwards approval on
//!   the continuation (issue #617).
//! * **Dry runs are not gated.** Every effect is stubbed
//!   ([`dry_run`](super::caps)), so there is nothing to approve, and pausing
//!   would stop a dry run from walking the rest of the graph — which is the one
//!   thing it exists to do.

use serde_json::{Value, json};
use tinyflows::model::{NodeKind, WorkflowGraph};

use oh::agent::tool_policy::{ToolCallContext, ToolPolicy, ToolPolicyDecision, ToolPolicyRequest};
use openhuman_core as oh;

use crate::company::Policy;
use crate::harness::policy::{ApprovalPolicy, ApprovalRequestQueue, ApprovalScope};
use crate::ports::types::{CompanyId, CompanyRecord};
use crate::runtime::grants::GrantSet;

/// The tool name an `http_request` node's call is classified as (issue #614).
///
/// The node kind has no slug of its own — it is not a `tool_call` — but the
/// call it makes is the same one `WorkflowToolInvoker` would run under this
/// name, and [`consequence_of`](crate::policy::consequence_of) already
/// classifies it (`Reach::Consequence`). Naming it here is what makes the two
/// node kinds answer to the same declaration instead of drifting.
const HTTP_REQUEST_TOOL: &str = "http_request";

/// One node the company's policy stopped, and why.
///
/// Carried from the gate pass to [`park_pending_gates`](super::runner) so the
/// operator's card can name the call and the reason rather than a bare node id
/// — the complaint #468 makes about the Approvals tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GatedCall {
    /// The node the engine will pause on.
    pub node_id: String,
    /// The tool this node would have run — a `tool_call` node's slug, or
    /// [`HTTP_REQUEST_TOOL`] for an `http_request` node.
    pub slug: String,
    /// The policy's own words for why it stopped — the same string the agent
    /// path puts on its card.
    pub reason: String,
    /// What the call would reach, when that is knowable and worth showing:
    /// `"POST api.example.com"` for an `http_request` node (issue #614), so the
    /// operator is deciding about a destination rather than a node id.
    ///
    /// **Method and host only, never the path or query.** A URL's query string
    /// is a routine place for tokens and signed parameters to sit, and this
    /// string is written to the durable journal, rendered on the Approvals page
    /// and kept after the decision. The host is what the decision actually turns
    /// on; the full URL is already in the node's own config for anyone who needs
    /// it.
    pub target: Option<String>,
    /// The call's authored arguments — the `url` a `web_fetch` will fetch, the
    /// recipient a send will reach (issue #846).
    ///
    /// Carried verbatim and **redacted downstream**, at the same projection that
    /// redacts a chat card's payload, rather than filtered here: one denylist,
    /// one set of bounds, and no second rule for this surface to drift away from
    /// (the discipline `crate::runtime::approval_display` records).
    ///
    /// Distinct from [`target`](Self::target) rather than derived from it,
    /// because they answer different questions and are governed differently:
    /// `target` is a one-line destination written to the journal and kept after
    /// the decision, which is why it is host-only and never a path or query;
    /// this is the call itself, shown so the operator can decide.
    pub args: Value,
}

/// Marks every `tool_call` node whose call the company's [`ApprovalPolicy`]
/// would park, so the engine stops the run there.
///
/// Returns what was gated, in graph order. An empty result leaves the graph
/// byte-identical to what [`translate`](super::translate) produced, so a `full`
/// company — and every existing test — runs exactly as it did before.
///
/// # The policy instance is deliberately unshared
///
/// [`ApprovalPolicy::check`] has a side effect: a `RequireApproval` verdict
/// **pushes the projected effect onto its request queue** for the brain to park.
/// That is right on the agent path and wrong here — the shared queue is drained
/// by [`park_gated_calls`](super::caps) and the chat cycle, both of which would
/// park a *second*, tool-call-shaped card for the same decision, and that one
/// carries the un-redeemable grant described in this module's docs. So this
/// builds its own policy over the default private queue nobody drains
/// ([`ApprovalPolicy::new`]'s documented behaviour), takes the verdict, and
/// drops it. The gate card is the only card.
///
/// Leaving the agent unbound is what makes the grant arms fail closed: both
/// [`consume_grant`] and `standing_grant_allows` short-circuit on a policy with
/// no agent, which is precisely the synthetic-principal decision this module's
/// docs record. The context below names that principal so a log line says which
/// workflow asked.
///
/// # Why it declares the authored-node path (issue #674)
///
/// [`for_authored_workflow_nodes`](ApprovalPolicy::for_authored_workflow_nodes)
/// scopes one arm — the per-call judgement #338 added — and nothing else.
///
/// A node reaching this pass has passed **two operator gates**: the company's
/// `[tools].allow` grant, which [`WorkflowToolInvoker`](super::caps) checks
/// fail-closed, and authoring, which refuses a namespace the invoker does not
/// wire. The operator saw the call. An agent turn has passed neither — the model
/// picks the tool and the arguments at run time — so #338's rules stay in force
/// there and #614's position holds here. #674 ruled the split.
///
/// The boundary condition lives in [`judge`](crate::policy::judge) rather than
/// here, because it is about the call and not about the caller: a node whose
/// arguments are templated from an upstream node's output was never
/// pre-declared, so it is judged as an agent call. This pass is exactly where
/// that matters — `every_reachable_workflow_tool_is_classified_by_name_alone`
/// records that node args may still be unresolved `=`-expressions when it runs.
///
/// The policy instance is private to this pass, so the declaration cannot leak
/// onto the agent path: no other caller shares it, and
/// [`ApprovalPolicy::new`] yields the strict path.
#[allow(dead_code)] // retained for audit/migration tests while production HITL is disabled
pub(crate) async fn apply_policy_gates(
    graph: &mut WorkflowGraph,
    record: &CompanyRecord,
    workflow_id: &str,
    run_id: &str,
    grants: &GrantSet,
) -> Vec<GatedCall> {
    apply_policy_gates_with_policy(
        graph,
        &record.effective_policy(),
        &record.id,
        workflow_id,
        run_id,
        grants,
    )
    .await
}

/// Applies the workflow policy gate using the effective policy already selected
/// for this run.
///
/// A `sub_workflow` child is translated later by the resolver, after the
/// top-level runner has applied its pass. The resolver has the run's effective
/// policy and grants but not the original [`CompanyRecord`], so it uses this
/// shared mutation half rather than reimplementing policy classification.
pub(crate) async fn apply_policy_gates_with_policy(
    graph: &mut WorkflowGraph,
    policy: &Policy,
    company: &CompanyId,
    workflow_id: &str,
    run_id: &str,
    grants: &GrantSet,
) -> Vec<GatedCall> {
    let gated = policy_gates(graph, policy, company, workflow_id, run_id, Some(grants)).await;

    // Written LAST and unconditionally: the policy's gate outranks an authored
    // `requires_approval`, in both directions. An author may add a gate the
    // policy does not require; they may not remove one it does.
    for node in &mut graph.nodes {
        if gated.iter().any(|call| call.node_id == node.id)
            && let Value::Object(config) = &mut node.config
        {
            config.insert("requires_approval".to_string(), json!(true));
        }
    }

    gated
}

/// Production migration mode: do not add policy-generated HITL gates.
///
/// Authored `requires_approval = true` flags already present on `graph` are
/// deliberately untouched; they are explicit workflow design, not a policy
/// interception. The returned policy metadata is empty because no policy gate
/// was added.
pub(crate) fn policy_hitl_disabled(_graph: &mut WorkflowGraph) -> Vec<GatedCall> {
    Vec::new()
}

/// Which of `graph`'s nodes the company's policy would stop — **classification
/// only**, no mutation.
///
/// `grants` is the company's live permission set (issue #1098). The top-level
/// runner and the `sub_workflow` resolver both pass `Some`, so a standing
/// permission the operator gave this workflow is honoured consistently at every
/// nesting level.
pub(crate) async fn policy_gates(
    graph: &WorkflowGraph,
    company_policy: &Policy,
    company: &CompanyId,
    workflow_id: &str,
    run_id: &str,
    grants: Option<&GrantSet>,
) -> Vec<GatedCall> {
    // The manifest's `[policy]` verbatim — same mode, same `always_approve`,
    // same `auto_approve_under_usd` the roster runs under. No per-agent budget:
    // a workflow node is not a teammate, and the company-wide ceiling is
    // enforced elsewhere.
    //
    // Issue #1098: bound to the workflow, so the standing-permission arm has a
    // subject to match on. Built **once** for the whole graph rather than per
    // node — the subject is the workflow, and every node of it spends the same
    // permission. The agent stays unbound, so the single-use arm stays closed.
    //
    // The queue is private-but-grant-sharing for the reason the module note
    // gives: `check` pushes its projected effect onto `requests`, and a shared
    // queue would have `park_gated_calls` or the chat cycle drain it into a
    // second, tool-call-shaped card for a decision this pass already carded.
    // The grants half must still be the company's real one or the permission is
    // invisible to the pass that has to honour it.
    let policy = ApprovalPolicy::new(company_policy, None)
        .for_authored_workflow_nodes()
        .with_workflow(workflow_id);
    let requests = match grants {
        Some(grants) => ApprovalRequestQueue::with_grants(grants.clone()),
        None => ApprovalRequestQueue::default(),
    };
    let policy = policy.with_requests(requests.clone());
    let claim = requests.claim(ApprovalScope::Run(format!("{run_id}::gate")));
    Box::pin(claim.scoped(judge_nodes(graph, &policy, company, workflow_id, run_id))).await
}

async fn judge_nodes(
    graph: &WorkflowGraph,
    policy: &ApprovalPolicy,
    company: &CompanyId,
    workflow_id: &str,
    run_id: &str,
) -> Vec<GatedCall> {
    let mut gated = Vec::new();

    for node in &graph.nodes {
        let Some((slug, args, target)) = call_of(node) else {
            continue;
        };

        // Cloned because the card wants the same arguments the policy judged
        // (issue #846), and `ToolPolicyRequest` takes them by value.
        let card_args = args.clone();
        let request = ToolPolicyRequest::new(
            &slug,
            args,
            ToolCallContext::session(
                run_id,
                "workflow",
                workflow_principal(workflow_id),
                &node.id,
                0,
            ),
        );
        // Only `RequireApproval` gates — see the module docs on `Deny`.
        let ToolPolicyDecision::RequireApproval { reason } = policy.check(&request).await else {
            continue;
        };

        tracing::debug!(
            company = %company,
            workflow = workflow_id,
            node = %node.id,
            tool = %slug,
            "workflow: the company's policy stops this node's call for an operator"
        );
        gated.push(GatedCall {
            node_id: node.id.clone(),
            slug,
            reason,
            target,
            args: card_args,
        });
    }

    gated
}

/// The call a node would make, as `(tool, args, target)` — or `None` for a node
/// kind that makes no classifiable call.
///
/// Two node kinds reach an outside-world capability without a turn behind them,
/// and both are read here so they answer to one declaration:
///
/// * **`tool_call`** (issue #460) — the authored `slug` and its `args`.
/// * **`http_request`** (issue #614) — a different capability
///   ([`GuardedHttpClient`](super::caps), never `ToolInvoker`) making the same
///   call the toolbelt's `http_request` tool makes, so it is classified under
///   that name. The whole node config is handed over as the arguments, which is
///   what the descriptor already is: `{ method, url, headers, body }`.
///
/// An `agent` node is deliberately absent: its gated calls already park through
/// #395's drain, and gating it here would park the same call twice.
fn call_of(node: &tinyflows::model::Node) -> Option<(String, Value, Option<String>)> {
    match node.kind {
        NodeKind::ToolCall => {
            // A `tool_call` without a slug fails in the engine's own node with a
            // clear error; there is no call to classify, so leave it be.
            let slug = node.config.get("slug").and_then(Value::as_str)?;
            let args = node
                .config
                .get("args")
                .cloned()
                .unwrap_or_else(|| json!({}));
            // Issue #846: the destination, when the arguments name one. #614
            // gave `http_request` a target and left `tool_call` without one,
            // which is why a parked `web_fetch` card named no host — the very
            // thing the operator is deciding about.
            let target = tool_target(&args);
            Some((slug.to_string(), args, target))
        }
        NodeKind::HttpRequest => Some((
            HTTP_REQUEST_TOOL.to_string(),
            node.config.clone(),
            http_target(&node.config),
        )),

        // Everything below reaches nothing outward *on this path*, and the match
        // is exhaustive on purpose: a wildcard here is how the third effectful
        // node kind ships ungated. Two PRs have each closed one hole in this
        // match (#460 for `tool_call`, #614 for `http_request`) and neither
        // announced itself — the second was found by reading, not by a failing
        // test. Listing every variant costs one line each and turns the
        // fourteenth kind into a compiler error at exactly the moment somebody
        // has to decide.

        // Runs a turn. Its gated calls already park through #395's drain, so
        // gating here would park the same call twice.
        NodeKind::Agent
        // Capabilities that are explicit stubs today: `CodeRunner`,
        // `MemoryProvider`, the `ShellRunner` (new in tinyflows 0.6.1), and the
        // `ApprovalProvider` (new in a later tinyflows 0.8.x) are wired to
        // error / left `None` (see `caps`), so there is no call to classify.
        // `Approval` falls back to pausing the run for the host to settle
        // through `engine::resume` rather than reaching anywhere on its own,
        // which is why it belongs in this group rather than being classified
        // like `tool_call`/`http_request`. **These are the next four to
        // gate** — sandboxed code, a memory *write*, a shell script, and a
        // wired approval notification are all effectful — and the decision
        // belongs with whoever wires the capability, in the same PR.
        | NodeKind::Code
        | NodeKind::Memory
        | NodeKind::Shell
        | NodeKind::Approval
        // A child graph is resolved and run *inside* the engine
        // (`run_sub_workflow`), so its nodes never pass this function at all —
        // this arm is not what excludes them. The module docs give the reason
        // it stays that way for now, and issue #617 tracks closing it: today a
        // call the policy parks at the top level runs unparked one level down.
        // The capability-level grant check still applies to a child's calls.
        | NodeKind::SubWorkflow
        // Pure control flow and data shaping. They reach no capability: the
        // decision belongs to the node they feed, which is classified above.
        | NodeKind::Condition
        | NodeKind::Dedup
        | NodeKind::Spawn
        | NodeKind::Scatter
        | NodeKind::Gather
        | NodeKind::Gate
        | NodeKind::Loop
        | NodeKind::Merge
        | NodeKind::OutputParser
        | NodeKind::SplitOut
        | NodeKind::Switch
        | NodeKind::Transform
        | NodeKind::Trigger
        // Terminal sink: discards its input and activates nothing further.
        // Pure control flow, like the group above.
        | NodeKind::Void => None,
    }
}

/// `"POST api.example.com"` for an `http_request` node's config — method and
/// host only, for the reasons on [`GatedCall::target`].
///
/// # Credentials never reach the card
///
/// A URL may carry userinfo — `https://token:secret@api.example.com/v1` — and
/// the authority is everything between the scheme and the first `/`, so taking
/// it whole would put that token in the durable approval journal and on the
/// Approvals page. Worse, it would show the operator the *wrong* host: a reader
/// scanning `POST token:secret@api.example.com` sees the credential first. Only
/// the part after the last `@` is kept, which is the host by definition — `@`
/// is not legal in a host. This is the same test the path and query already
/// failed; userinfo is one more place a secret sits.
///
/// # A destination that is not knowable yet says so
///
/// `url = "=item.endpoint"` is routine authoring: the destination is an
/// expression the run has not resolved when this pass runs. Returning `None`
/// there would render an `http_request` card identically to a `tool_call` one —
/// a call with no destination to show — and an operator approving an outbound
/// request whose host is unknown should be told that is what they are doing.
/// So an unresolvable URL yields `"POST (destination resolved at run time)"`:
/// still never a guess, but the absence is stated rather than implied. A
/// **missing** `url` key stays `None` — nothing was authored, so there is
/// nothing to explain.
/// The host a `tool_call` node's arguments name, when they name one
/// (issue #846).
///
/// **Host only, never the path or query**, on exactly [`http_target`]'s terms
/// and for exactly its reason: this string is written to the durable journal,
/// rendered on the Approvals page and kept after the decision, and a URL's query
/// is a routine place for tokens and signed parameters to sit. The full
/// arguments travel separately on [`GatedCall::args`], where they are redacted
/// by the shared projection before they reach a console.
///
/// No method, because a `tool_call` has none to state — that is `http_request`'s
/// vocabulary, and borrowing it would put a `GET` on a card for a call that is
/// not an HTTP request.
///
/// Reads `url` only. Widening this to "any argument that looks like a URL" would
/// mean guessing which of several is *the* destination, and a card that names
/// the wrong one is worse than a card that names none — the operator would
/// authorise against it.
fn tool_target(args: &Value) -> Option<String> {
    let url = args.get("url").and_then(Value::as_str)?;
    host_of(url)
}

/// The host component of `url`, or `None` when there is not one to read.
///
/// Shared by [`tool_target`] and [`http_target`] so the two surfaces cannot
/// disagree about what a host is — in particular about userinfo, which is
/// everything before the **last** `@` and which a host cannot contain.
fn host_of(url: &str) -> Option<String> {
    url.split_once("://")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .map(|authority| {
            authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host)
        })
        .filter(|host| !host.is_empty())
        .map(str::to_string)
}

fn http_target(config: &Value) -> Option<String> {
    let url = config.get("url").and_then(Value::as_str)?;
    let method = config
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_uppercase();
    // Userinfo handling lives in `host_of`, shared with `tool_target`.
    match host_of(url) {
        Some(host) => Some(format!("{method} {host}")),
        None => Some(format!("{method} (destination resolved at run time)")),
    }
}

/// What a paused node's card should say about the call it is stopping —
/// whoever raised the gate (issue #846).
///
/// # The hole this closes
///
/// [`GatedCall`] is produced by [`policy_gates`], so it exists only for a node
/// the **company's policy** stopped. A node the **author** stopped with
/// `requires_approval: true` produced no entry, and its card therefore carried
/// no tool, no arguments and no destination — just a node id and the engine's
/// resume payload. On a `full`-tier company, where the policy stops nothing,
/// that is *every* workflow card: the operator is asked to authorise
/// `fetch_bbc` and shown `{"items":[{"json":{}}],"port":null}`.
///
/// #372 made the same complaint about the chat surface and #375 fixed it there,
/// by carrying the effect's own arguments onto the card. The information was
/// already on the host in that case and it is already on the host in this one —
/// [`call_of`] has read the slug and the args since #460, and only the *reason*
/// was ever policy-specific. So this asks `call_of` the same question for a node
/// nobody's policy stopped, and the card gains everything except the sentence no
/// one wrote.
///
/// Returns `None` for a node the graph does not contain, or one whose kind makes
/// no classifiable call — an authored gate on a `transform` is a genuine "stop
/// and look at this", with no call to describe, and a card that invented one
/// would be worse than a card that says so.
pub(crate) fn describe_call(graph: &WorkflowGraph, node_id: &str) -> Option<GatedCall> {
    let node = graph.nodes.iter().find(|node| node.id == node_id)?;
    let (slug, args, target) = call_of(node)?;
    Some(GatedCall {
        node_id: node.id.clone(),
        slug,
        // Nobody stated one. The console says "the workflow's author asked for a
        // person here" in its own words rather than the host inventing a
        // policy-shaped sentence for a decision no policy made.
        reason: String::new(),
        target,
        args,
    })
}

/// The synthetic principal a workflow `tool_call` acts as.
///
/// Not a roster teammate, and deliberately shaped so it can never collide with
/// one: nothing mints a grant for it, so every grant arm in
/// [`ApprovalPolicy::check`] fails closed. It exists to make the policy's own
/// log lines name the workflow that asked. Mirrors the label
/// [`build_capabilities`](super::caps) already stamps on this run's search
/// metering.
fn workflow_principal(workflow_id: &str) -> String {
    format!("workflow:{workflow_id}")
}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod tests;
