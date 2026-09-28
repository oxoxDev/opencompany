//! Tests for the planning station (issue #337).
//!
//! Two tiers, and the split is deliberate.
//!
//! The **unit** tier covers the pure decisions — the parse, the caps, the path
//! render, and every arm of the verification table — because those are where a
//! wrong answer is silent: a prerequisite stamped `satisfied` when it should
//! have been `missing` produces a card that dispatches into work it cannot do,
//! and nothing anywhere reports an error.
//!
//! The **pass** tier runs the real [`run_planning_pass`] against a real
//! [`CompanyRuntime`] with a real store and a scripted model, because the three
//! things most likely to be wrong — that the plan lands, that the card lands in
//! the right column, and that a discarded pass leaves the board alone — are all
//! properties of the whole pass and cannot be seen from any of its parts.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use tinyinference::model::{ChatModel, ModelResponse};
use tinyinference::usage::Usage;
use tinyinference::{Error as InferenceError, Result as TaResult};

use super::*;
use crate::company::CompanyManifest;
use crate::ports::types::CompanyId;

// ---------------------------------------------------------------------------
// A scripted model
// ---------------------------------------------------------------------------

/// A model that answers with a canned string (or fails), counts its calls, and
/// records the prompt it was given.
///
/// The prompt capture is not incidental: two of the tests below assert on what
/// the model was *shown*, which is the only way to check that the pass hands it
/// no secret and no tool.
pub(crate) struct ScriptedModel {
    reply: Option<String>,
    /// Token usage the call reports, mirrored onto the [`ModelResponse`] so a
    /// test can control what [`record_usage`] charges for this call.
    usage: Option<Usage>,
    calls: AtomicUsize,
    prompts: StdMutex<Vec<String>>,
    /// Simulates a provider that never answers, for the timeout path.
    hang: bool,
}

impl ScriptedModel {
    pub(crate) fn replying(reply: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            reply: Some(reply.into()),
            usage: None,
            calls: AtomicUsize::new(0),
            prompts: StdMutex::new(Vec::new()),
            hang: false,
        })
    }

    /// Same as [`Self::replying`], but the response carries `usage` — for
    /// tests that need [`record_usage`] to charge a specific token amount.
    pub(crate) fn replying_with_usage(reply: impl Into<String>, usage: Usage) -> Arc<Self> {
        Arc::new(Self {
            reply: Some(reply.into()),
            usage: Some(usage),
            calls: AtomicUsize::new(0),
            prompts: StdMutex::new(Vec::new()),
            hang: false,
        })
    }

    pub(crate) fn failing() -> Arc<Self> {
        Arc::new(Self {
            reply: None,
            usage: None,
            calls: AtomicUsize::new(0),
            prompts: StdMutex::new(Vec::new()),
            hang: false,
        })
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub(crate) fn last_prompt(&self) -> String {
        self.prompts
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl ChatModel<()> for ScriptedModel {
    async fn invoke(&self, _state: &(), request: ModelRequest) -> TaResult<ModelResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompts.lock().unwrap().push(
            request
                .messages
                .iter()
                .map(|m| m.text())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        assert!(
            request.tools.is_empty(),
            "a planning pass must expose NO tools — a tool here is a loop, and a loop is a \
             dispatch"
        );
        if self.hang {
            // Longer than PLANNING_TIMEOUT could ever be waited for in a test;
            // the test that uses this shortens nothing and instead asserts the
            // deadline exists via `PLANNING_TIMEOUT`.
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
        match &self.reply {
            Some(reply) => {
                let response = ModelResponse::assistant(reply.clone());
                Ok(match self.usage {
                    Some(usage) => response.with_usage(usage),
                    None => response,
                })
            }
            None => Err(InferenceError::Model("the brain is down".to_string())),
        }
    }
}

impl HarnessModel for ScriptedModel {
    fn telemetry_provider_id(&self) -> String {
        "managed".to_string()
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

pub(crate) const MANIFEST: &str = r#"
[company]
name = "Acme"

[[agent]]
id = "maya"
role = "Writer"
tools = ["docs", "web"]

[[agent]]
id = "sam"
role = "Engineer"
tools = ["code"]

[[group_chat]]
id = "studio"
name = "Studio"
members = ["maya"]

[[group_chat]]
id = "empty_desk"
name = "Nobody"

[[connection]]
provider = "github"

[[connection]]
provider = "slack"

[policy]
mode = "full"

[tools]
allow = ["docs", "web", "code"]
"#;

pub(crate) fn manifest() -> CompanyManifest {
    toml::from_str(MANIFEST).expect("the fixture manifest parses")
}

pub(crate) fn record() -> CompanyRecord {
    CompanyRecord {
        general_channel: Default::default(),
        overlay_desk_hive: Vec::new(),
        overlay_retired_agents: Vec::new(),
        overlay_agent_edits: Vec::new(),
        id: CompanyId::new("acme"),
        manifest: manifest(),
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

/// A hand-built evidence pack, so each verification arm can be exercised
/// against an exactly-known inventory.
pub(crate) fn evidence() -> Evidence {
    let record = record();
    let allow = record.manifest.tools.allow.clone();
    let teammates = record
        .manifest
        .agents
        .iter()
        .map(|a| TeammateBrief {
            id: a.id.clone(),
            role: a.role.clone(),
            description: a.description.clone(),
            grants: crate::runtime::builder::agent_effective_grants(&allow, a.tools.as_deref()),
            global: a.global,
        })
        .collect();
    Evidence {
        company_name: "Acme".to_string(),
        policy_mode: record.manifest.policy.mode.clone(),
        always_approve: Vec::new(),
        record,
        card_title: "Ship the changelog".to_string(),
        card_note: None,
        card_priority: "medium".to_string(),
        card_assignee: "maya".to_string(),
        teammates,
        desks: vec![("studio".to_string(), vec!["maya".to_string()])],
        connections: HashMap::from([
            (
                "github".to_string(),
                (true, vec!["native".to_string()], false),
            ),
            ("slack".to_string(), (false, Vec::new(), false)),
            (
                "notion".to_string(),
                (true, vec!["composio".to_string()], false),
            ),
        ]),
        composio_reachable: true,
        mcp_servers: HashMap::from([("search".to_string(), true), ("legacy".to_string(), false)]),
        workspace: vec![
            "standards/Tone.md".to_string(),
            "playbooks/Launch.md".to_string(),
        ],
        skills: vec!["writing".to_string()],
        mail_configured: false,
        composio_credential: true,
        native_capabilities: HashSet::new(),
        search_backend_configured: false,
        media_backend_configured: false,
    }
}

/// Issue #982: the gate keeps its promise now that the assignee it is being
/// handed usually came from the operator addressing a thread.
///
/// The pairing is the test. A blank card takes the planner's content-derived
/// guess — which is the behaviour every unaddressed card still wants — and a
/// card that already names somebody keeps them even when the planner proposed a
/// different, perfectly plausible teammate. That second row is the whole of the
/// bug: the addressee is written before the pass runs, so the pass has to be the
/// thing that does not overwrite it.
#[test]
fn the_gate_fills_a_blank_assignee_and_never_overrules_one() {
    assert_eq!(
        settled_assignee("", Some("maya".to_string())).as_deref(),
        Some("maya"),
        "a blank card is what the planner's proposal is for"
    );
    assert_eq!(
        settled_assignee("sam", Some("maya".to_string())).as_deref(),
        Some("sam"),
        "an assignee the operator chose outranks a content match"
    );
    assert_eq!(
        settled_assignee("sam", None).as_deref(),
        Some("sam"),
        "…and stands on its own when the planner proposed nobody"
    );
    assert_eq!(
        settled_assignee("", None),
        None,
        "nobody either way still blocks, exactly as before"
    );
}

/// …and the assignee a chat card now arrives with is one the gate accepts, so
/// nothing newly settles as blocked.
///
/// Both shapes the chat route can write are checked: a teammate id, and a
/// **desk** id, which is what a desk-addressed message opens its card with.
#[test]
fn an_addressed_chat_cards_assignee_passes_the_gates_validity_check() {
    let e = evidence();
    assert_eq!(e.card_assignee, "maya", "the fixture card names a teammate");
    assert!(
        e.assignee_is_valid(&e.card_assignee),
        "a teammate-addressed card dispatches rather than blocking"
    );
    assert!(
        e.assignee_is_valid("studio"),
        "a desk-addressed card carries the desk id, and that is valid too"
    );
    assert!(
        !e.assignee_is_valid("nobody_by_that_name"),
        "…and the check still refuses a name nobody answers to"
    );
}

pub(crate) fn claim(kind: PrereqKind, name: &str) -> PrereqClaim {
    PrereqClaim {
        kind,
        name: name.to_string(),
        why: String::new(),
    }
}

/// A well-formed model answer that needs nothing.
pub(crate) const CLEAN_PLAN: &str = r#"```json
{
  "description": "Write the changelog entry for the release.",
  "steps": [{"title": "Draft it", "detail": "Against the tagged version", "estimatedMinutes": 15}],
  "prerequisites": [],
  "risks": ["the tag may not exist yet"],
  "verification": "the entry is in the file and reads correctly",
  "scope": "the changelog only",
  "assigneeCandidates": [{"id": "maya", "reason": "writes everything the company ships"}]
}
```"#;

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Models fence their JSON and narrate around it. Both are tolerated; neither
/// changes what is extracted.
#[test]
fn a_fenced_or_narrated_answer_still_parses() {
    let fenced = parse_draft(CLEAN_PLAN).expect("a fenced answer parses");
    assert_eq!(fenced.steps.len(), 1);
    assert_eq!(fenced.assignee_candidates.len(), 1);
    assert_eq!(fenced.assignee_candidates[0].id, "maya");

    let narrated = parse_draft(
        "Sure! Here is the plan:\n{\"description\":\"do it\",\"steps\":[]}\nLet me know.",
    )
    .expect("a narrated answer parses");
    assert_eq!(narrated.description, "do it");
}

/// Strict parse or nothing. A plan whose structure was *guessed* from prose is
/// exactly the plan with an empty prerequisite list — which is exactly the plan
/// that dispatches when it should have stopped. So prose is a failure, and the
/// pass returns the card rather than inventing a brief.
#[test]
fn prose_is_a_failure_not_a_description() {
    assert!(parse_draft("I think we should start by writing the entry.").is_none());
    assert!(parse_draft("").is_none());
    assert!(parse_draft("{ not json at all }").is_none());
    assert!(parse_draft("}{").is_none());
}

/// A model **cannot** assert a verdict. The claim type has no `status` field,
/// so one emitted on the wire is dropped by the parse rather than trusted — the
/// asymmetry is enforced by the type, not by the prompt asking nicely.
#[test]
fn a_model_supplied_status_is_not_deserialized() {
    let draft = parse_draft(
        r#"{"description":"d","steps":[],"prerequisites":[
             {"kind":"connection","name":"slack","status":"satisfied","why":"posting"}]}"#,
    )
    .expect("parses");
    assert_eq!(draft.prerequisites.len(), 1);
    assert_eq!(draft.prerequisites[0].kind, PrereqKind::Connection);
    // The host then stamps the real verdict, which is the opposite of the claim.
    let (status, _) = verify_connection(&evidence(), "slack");
    assert_eq!(status, PrereqStatus::Missing);
}

// ---------------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------------

/// The caps cut on a character boundary. A multi-byte brief must not panic the
/// pass or persist a split codepoint.
#[test]
fn caps_are_codepoint_safe() {
    let long = "é".repeat(MAX_LABEL_CHARS + 50);
    let capped = cap(&long, MAX_LABEL_CHARS);
    assert_eq!(
        capped.chars().count(),
        MAX_LABEL_CHARS + 1,
        "plus the ellipsis"
    );
    assert!(capped.ends_with('…'));
    assert_eq!(cap("  tidy  ", 100), "tidy");
}

/// Logical paths are rendered from the parent chain, and a corrupt tree
/// terminates instead of hanging the pass.
#[test]
fn workspace_paths_render_and_terminate() {
    use crate::ports::workspace::{NodeKind, WorkspaceNode, WorkspaceOrigin};
    let node = |id: &str, name: &str, parent: Option<&str>, kind| WorkspaceNode {
        id: id.to_string(),
        name: name.to_string(),
        kind,
        parent_id: parent.map(str::to_string),
        updated_at_millis: 0,
        created_by: WorkspaceOrigin::Operator,
        updated_by: WorkspaceOrigin::Operator,
        mime: None,
        size: None,
        sha256: None,
        adopted: false,
    };
    let paths = workspace_paths(vec![
        node("1", "standards", None, NodeKind::Folder),
        node("2", "tone.md", Some("1"), NodeKind::File),
        node("3", "readme.md", None, NodeKind::File),
    ]);
    assert_eq!(paths, vec!["readme.md", "standards", "standards/tone.md"]);

    // A cycle is not a reachable state, but it must not be an infinite loop.
    let cyclic = workspace_paths(vec![
        node("a", "A", Some("b"), NodeKind::Folder),
        node("b", "B", Some("a"), NodeKind::Folder),
    ]);
    assert_eq!(cyclic.len(), 2);
}

// ---------------------------------------------------------------------------
// Verification — every arm of the table
// ---------------------------------------------------------------------------

#[test]
fn a_connection_is_checked_against_the_inventory() {
    let e = evidence();
    assert_eq!(verify_connection(&e, "notion").0, PrereqStatus::Satisfied);
    // Case is not a distinction an operator should have to get right.
    assert_eq!(verify_connection(&e, "Notion").0, PrereqStatus::Satisfied);
    assert_eq!(verify_connection(&e, "slack").0, PrereqStatus::Missing);
    let (status, note) = verify_connection(&e, "stripe");
    assert_eq!(status, PrereqStatus::Missing, "undeclared reads as missing");
    assert!(note.contains("Connections tab"), "{note}");
}

/// **The arm this whole check exists for.** A provider connected *natively* is
/// stored under the host's own `oauth/{provider}` namespace, which no agent
/// tool ever reads. Reporting `satisfied` would green-light a card that
/// dispatches into work it cannot do and fails with no explanation — the exact
/// silent-wrong-answer this module's tests are here to catch (issue #396).
///
/// The note must **acknowledge** the stored connection rather than claim the
/// provider is not connected: an operator who just completed that OAuth
/// handshake, told to "connect it", will connect it again and get nowhere.
#[test]
fn a_natively_connected_provider_is_missing_not_satisfied() {
    let mut e = evidence();
    e.connections.insert(
        "github".to_string(),
        (true, vec!["native".to_string()], false),
    );
    let (status, note) = verify_connection(&e, "github");
    assert_eq!(
        status,
        PrereqStatus::Missing,
        "a native-only credential confers no agent capability: {note}"
    );
    assert!(
        note.contains("Composio"),
        "the note must name the path that does work: {note}"
    );
    assert!(
        note.contains("is connected in this host's catalog"),
        "the note must acknowledge the credential the operator already stored, \
         not tell them to connect it again: {note}"
    );

    // An empty `via` is the same verdict for the same reason — nothing in it
    // names a path a tool can resolve.
    e.connections
        .insert("github".to_string(), (true, Vec::new(), false));
    assert_eq!(verify_connection(&e, "github").0, PrereqStatus::Missing);
}

/// The satisfying case, and the only one: a Composio-backed connection is the
/// single path a tool actually resolves a credential from.
#[test]
fn a_composio_backed_connection_is_satisfied() {
    let mut e = evidence();
    e.connections.insert(
        "github".to_string(),
        (true, vec!["composio".to_string()], false),
    );
    let (status, note) = verify_connection(&e, "github");
    assert_eq!(status, PrereqStatus::Satisfied, "{note}");
    assert!(note.contains("composio"), "{note}");
}

/// Both namespaces at once is still satisfied. The check is membership, not
/// equality — a provider connected natively *and* through Composio has a
/// credential a tool can reach, and the useless native copy alongside it does
/// not take that away.
#[test]
fn a_connection_via_both_namespaces_is_satisfied() {
    let mut e = evidence();
    e.connections.insert(
        "github".to_string(),
        (
            true,
            vec!["native".to_string(), "composio".to_string()],
            false,
        ),
    );
    assert_eq!(verify_connection(&e, "github").0, PrereqStatus::Satisfied);

    // And in the other order, because a `via` list has no guaranteed ordering.
    e.connections.insert(
        "github".to_string(),
        (
            true,
            vec!["composio".to_string(), "native".to_string()],
            false,
        ),
    );
    assert_eq!(verify_connection(&e, "github").0, PrereqStatus::Satisfied);
}

/// `unverified` outranks the `via` distinction in both directions. A probe that
/// did not answer cannot tell us *how* a provider is connected any more than it
/// can tell us *whether* — so the verdict is `unknown`, never the new `missing`.
#[test]
fn an_unverified_row_is_unknown_whatever_its_via_says() {
    let mut e = evidence();
    for via in [
        Vec::new(),
        vec!["native".to_string()],
        vec!["composio".to_string()],
        vec!["native".to_string(), "composio".to_string()],
    ] {
        e.connections
            .insert("github".to_string(), (true, via.clone(), true));
        let (status, note) = verify_connection(&e, "github");
        assert_eq!(
            status,
            PrereqStatus::Unknown,
            "via {via:?} on an unverified row: {note}"
        );
    }
}

/// The failure direction that matters. A provider whose inventory could not be
/// reached is **unknown**, never **missing** — a Composio outage must not make
/// every card in the company unplannable.
#[test]
fn an_unreachable_inventory_is_unknown_never_missing() {
    let mut e = evidence();
    e.connections
        .insert("github".to_string(), (false, Vec::new(), true));
    assert_eq!(verify_connection(&e, "github").0, PrereqStatus::Unknown);

    // Same for a provider that is simply absent while the probe was down: we
    // cannot tell "not connected" from "we could not look".
    e.composio_reachable = false;
    assert_eq!(verify_connection(&e, "stripe").0, PrereqStatus::Unknown);
    assert_eq!(verify_composio(&e, "notion").0, PrereqStatus::Unknown);

    // And an MCP union that would not resolve leaves an empty map, which is
    // unknown rather than "no server by that name".
    let mut e = evidence();
    e.mcp_servers.clear();
    assert_eq!(verify_mcp(&e, "search").0, PrereqStatus::Unknown);

    // And an unlistable workspace.
    let mut e = evidence();
    e.workspace.clear();
    assert_eq!(
        verify_file(&e, "standards/Tone.md").0,
        PrereqStatus::Unknown
    );
}

#[test]
fn composio_distinguishes_no_credential_from_no_account() {
    let e = evidence();
    assert_eq!(verify_composio(&e, "notion").0, PrereqStatus::Satisfied);
    // Connected natively but NOT through Composio is not a Composio account.
    assert_eq!(verify_composio(&e, "github").0, PrereqStatus::Missing);

    let mut e = evidence();
    e.composio_credential = false;
    let (status, note) = verify_composio(&e, "gmail");
    assert_eq!(status, PrereqStatus::Missing);
    assert!(
        note.contains("no Composio credential"),
        "the operator needs to know which of the two things is missing: {note}"
    );
}
