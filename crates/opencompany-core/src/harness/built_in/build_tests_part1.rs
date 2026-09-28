use super::*;

#[tokio::test]
async fn memory_tools_are_wired_to_the_company_context_store() {
    // The flip of the old withholding lock. The doc comment on
    // `memory_tools` demanded that whatever un-withholds these must first
    // confirm each company's own `ContextStore` genuinely backs them — so
    // that is exactly what this asserts: a store through the tool lands in
    // THIS company's context rows, under the agent's own label prefix,
    // reachable by the same port the memory_loop and the Brain view read.
    use crate::ports::ContextStore;
    use crate::ports::types::CompanyId;
    use std::sync::Arc;

    let dir = tempfile::tempdir().expect("tempdir");
    let context: Arc<dyn ContextStore> =
        Arc::new(crate::store::FsContextStore::new(dir.path().to_path_buf()));
    let company = CompanyId::new("acme");
    let tools = crate::harness::built_in::memory_tools::memory_tools(
        context.clone(),
        company.clone(),
        "ceo".to_string(),
    );
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(names, ["memory_store", "memory_recall", "memory_forget"]);

    let store = &tools[0];
    let reply = store
        .execute(serde_json::json!({"title": "Pin", "body": "the fact"}))
        .await
        .expect("execute");
    assert!(!reply.is_error, "{reply:?}");
    let rows = context.list(&company, "agent-memory/ceo/").await.unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the tool write must land on the company port"
    );
    assert_eq!(rows[0].label, "agent-memory/ceo/pin");
}

/// The grant alone is not enough: a wired `shell`/`code` namespace the
/// capability tier denies must not be described in the sandbox brief,
/// because `filter_by_capabilities` is about to strip the matching tools
/// from the vector handed to the builder. This is the fix for the P1
/// codex found on PR #1670 — before it, `sandbox_brief_flags` did not
/// exist and the brief was built from the grant flags alone.
#[test]
fn sandbox_brief_flags_withhold_a_capability_denied_namespace() {
    use std::collections::HashSet;

    let deny_shell = toolbelt::CapabilityFilter::DenyNamespaces(HashSet::from(["shell"]));
    assert_eq!(
        sandbox_brief_flags(true, true, true, &deny_shell),
        (true, false, true),
        "a denied `shell` must not be reported even though it was wired"
    );

    let deny_code = toolbelt::CapabilityFilter::DenyNamespaces(HashSet::from(["code"]));
    assert_eq!(
        sandbox_brief_flags(true, true, true, &deny_code),
        (true, true, false),
        "a denied `code` must not be reported even though it was granted"
    );

    let deny_both = toolbelt::CapabilityFilter::DenyNamespaces(HashSet::from(["shell", "code"]));
    assert_eq!(
        sandbox_brief_flags(true, true, true, &deny_both),
        (true, false, false)
    );
}

/// The identity filter changes nothing — the flags are exactly the wired
/// grant flags, files included (files are never a gateable namespace).
#[test]
fn sandbox_brief_flags_pass_through_under_allow_all() {
    assert_eq!(
        sandbox_brief_flags(true, true, true, &toolbelt::CapabilityFilter::AllowAll),
        (true, true, true)
    );
    assert_eq!(
        sandbox_brief_flags(false, false, false, &toolbelt::CapabilityFilter::AllowAll),
        (false, false, false)
    );
}

/// An ungranted/unwired namespace stays absent regardless of the capability
/// filter — denial can only ever narrow, never widen, what the grant wired.
#[test]
fn sandbox_brief_flags_never_add_a_namespace_the_grant_did_not_wire() {
    use std::collections::HashSet;

    let allow_all = toolbelt::CapabilityFilter::AllowAll;
    assert_eq!(
        sandbox_brief_flags(false, false, false, &allow_all),
        (false, false, false)
    );

    // Denying a namespace that was never wired is a no-op on that flag.
    let deny_shell = toolbelt::CapabilityFilter::DenyNamespaces(HashSet::from(["shell"]));
    assert_eq!(
        sandbox_brief_flags(false, false, false, &deny_shell),
        (false, false, false)
    );
}

#[test]
fn grants_cover_matches_namespace_glob_and_star() {
    assert!(grants_cover(&["docs.*".into()], "docs"));
    assert!(grants_cover(&["docs".into()], "docs"));
    assert!(grants_cover(&["docs.read".into()], "docs"));
    assert!(grants_cover(&["*".into()], "docs"));
    assert!(!grants_cover(&["web.*".into()], "docs"));
    assert!(!grants_cover(&[], "docs"));
    // A prefix must end on a namespace boundary, not a substring.
    assert!(!grants_cover(&["documentation.*".into()], "docs"));
}

#[test]
fn file_tools_are_sandboxed_to_the_workspace() {
    let ws = Path::new("/tmp/agent-ws");
    let policy = workspace_security(ws);
    assert!(policy.workspace_only, "file tools must be workspace-only");
    assert_eq!(policy.workspace_dir, ws);
    assert_eq!(policy.action_dir, ws);

    let tools = file_tools(ws, None);
    assert_eq!(tools.len(), 6, "read/write/edit/list/grep/glob");
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"file_read"), "got {names:?}");
    assert!(names.contains(&"file_write"), "got {names:?}");
}

#[test]
fn ensure_agent_workspace_mints_the_whole_chain_and_is_idempotent() {
    let root = tempfile::tempdir().expect("tempdir");
    let company = CompanyId::new("acme");

    // Nothing under the root exists yet — not the company segment, not the
    // agent segment. This is a company that has never run.
    let named = agent_workspace(root.path(), &company, "ceo");
    assert!(!named.exists(), "precondition: nothing minted yet");

    let made = ensure_agent_workspace(root.path(), &company, "ceo").expect("first ensure");
    assert_eq!(made, named, "creation and naming must agree exactly");
    assert!(made.is_dir());

    // Idempotent: a second call on an existing tree is a success, not an
    // `AlreadyExists` error — the dispatch path calls this on every turn.
    let again = ensure_agent_workspace(root.path(), &company, "ceo").expect("second ensure");
    assert_eq!(again, made);
    assert!(again.is_dir());
}

/// The sandbox is named under the workspace naming rule, so a snake_case
/// roster id and an underscored company id land on dashed directories —
/// the same convention the note tree beside them is kept in.
#[test]
fn the_sandbox_path_is_lowercase_and_dashed() {
    let root = tempfile::tempdir().expect("tempdir");
    let named = agent_workspace(
        root.path(),
        &CompanyId::new("Agentic_Law Firm"),
        "page_builder",
    );

    assert_eq!(
        named,
        root.path()
            .join("agentic-law-firm")
            .join("page-builder")
            .join("workspace")
    );
}

/// An agent upgraded into the new naming keeps the work it had in flight.
///
/// The sandbox is private scratch that nothing outside this process
/// addresses, so moving it is invisible — while leaving it behind would
/// strand a half-finished file on disk, present and unreachable, with
/// nothing reporting it.
#[test]
fn a_pre_rule_sandbox_is_moved_onto_the_canonical_path() {
    let root = tempfile::tempdir().expect("tempdir");
    let company = CompanyId::new("acme");

    let legacy = root
        .path()
        .join("acme")
        .join("page_builder")
        .join("workspace");
    std::fs::create_dir_all(&legacy).expect("legacy sandbox");
    std::fs::write(legacy.join("draft.md"), "half-finished").expect("in-flight work");

    let made = ensure_agent_workspace(root.path(), &company, "page_builder").expect("ensure");

    assert_eq!(made, agent_workspace(root.path(), &company, "page_builder"));
    assert_eq!(
        std::fs::read_to_string(made.join("draft.md")).expect("the work came with it"),
        "half-finished"
    );
    assert!(!legacy.exists(), "the legacy path is not left as a twin");
}

/// A sandbox that already exists at the canonical path is never overwritten
/// by a stale legacy one — the move is a one-time adoption, not a sync.
#[test]
fn a_live_sandbox_is_never_replaced_by_a_legacy_one() {
    let root = tempfile::tempdir().expect("tempdir");
    let company = CompanyId::new("acme");

    let canonical = ensure_agent_workspace(root.path(), &company, "page_builder").expect("ensure");
    std::fs::write(canonical.join("current.md"), "live").expect("live work");
    let legacy = root
        .path()
        .join("acme")
        .join("page_builder")
        .join("workspace");
    std::fs::create_dir_all(&legacy).expect("legacy sandbox");
    std::fs::write(legacy.join("stale.md"), "stale").expect("stale work");

    let again = ensure_agent_workspace(root.path(), &company, "page_builder").expect("ensure");

    assert_eq!(again, canonical);
    assert_eq!(
        std::fs::read_to_string(canonical.join("current.md")).expect("still there"),
        "live"
    );
    assert!(!canonical.join("stale.md").exists());
}

/// The bug, pinned. With the workspace absent, `validate_parent_path` walks
/// up past it to an ancestor that really *is* outside the sandbox and
/// refuses a plainly-inside relative path — the refusal an agent granted
/// `files` but not `shell` used to hit on every write.
#[tokio::test]
async fn a_missing_workspace_makes_a_plain_relative_write_look_like_an_escape() {
    let root = tempfile::tempdir().expect("tempdir");
    let workspace = agent_workspace(root.path(), &CompanyId::new("acme"), "ceo");
    assert!(!workspace.exists(), "precondition: never provisioned");

    let policy = workspace_security(&workspace);
    let err = policy
        .validate_parent_path("notes.md")
        .await
        .expect_err("a missing workspace refuses the write");
    assert!(
        err.contains("escapes workspace"),
        "the guard blames traversal for a missing directory: {err}"
    );
}

/// The fix. The same policy over an *ensured* workspace resolves the same
/// relative path, inside the sandbox.
#[tokio::test]
async fn an_ensured_workspace_resolves_a_relative_write_inside_the_sandbox() {
    let root = tempfile::tempdir().expect("tempdir");
    let workspace =
        ensure_agent_workspace(root.path(), &CompanyId::new("acme"), "ceo").expect("ensure");

    let policy = workspace_security(&workspace);
    let resolved = policy
        .validate_parent_path("notes.md")
        .await
        .expect("an existing workspace accepts a relative write");

    let canonical = workspace.canonicalize().expect("canonicalize");
    assert!(
        resolved.starts_with(&canonical),
        "{} is not inside {}",
        resolved.display(),
        canonical.display()
    );
    assert_eq!(
        resolved.file_name().and_then(|n| n.to_str()),
        Some("notes.md")
    );

    // A nested path whose parent does not exist yet still resolves — the
    // guard only ever needed *some* existing ancestor inside the sandbox.
    let nested = policy
        .validate_parent_path("reports/q3/summary.md")
        .await
        .expect("a not-yet-created subdirectory still resolves");
    assert!(nested.starts_with(&canonical));
}

/// Provisioning does not loosen the guard: a genuine escape is still
/// refused — and it comes back **word for word** the same as the
/// missing-workspace refusal above. That is why #409 was filed rather than
/// closed by the one-line create.
///
/// Reaching the resolved-parent arm with a real escape takes some care,
/// which is itself part of the finding. A symlink whose *immediate* parent
/// resolves (`escape/loot.txt`) is caught earlier, by the string-level
/// symlink check, with a different and perfectly clear message. The arm
/// under test is reached only when no existing ancestor can be canonicalized
/// up front: a symlink out of the sandbox plus a not-yet-created
/// subdirectory under it. So in a `workspace_only` agent sandbox this
/// wording fires for exactly two conditions — a hostile symlink and a
/// workspace that was never created — and says "escapes workspace" for both.
///
/// Pinned here so a wording change upstream surfaces in this repo instead of
/// drifting silently.
#[cfg(unix)]
#[tokio::test]
async fn a_real_escape_and_a_missing_workspace_are_refused_in_identical_words() {
    let root = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let workspace =
        ensure_agent_workspace(root.path(), &CompanyId::new("acme"), "ceo").expect("ensure");
    std::os::unix::fs::symlink(outside.path(), workspace.join("escape")).expect("symlink");

    let policy = workspace_security(&workspace);

    // The easy half: the immediate parent resolves, so the string-level
    // symlink check refuses it first — clearly, and distinguishably.
    let shallow = policy
        .validate_parent_path("escape/loot.txt")
        .await
        .expect_err("a symlink out of the sandbox is refused");
    assert!(
        shallow.contains("Path not allowed by security policy"),
        "expected the string-level refusal: {shallow}"
    );

    // The arm this issue is about: nothing up front can be canonicalized,
    // so the ancestor walk runs and lands outside the sandbox.
    let deep = policy
        .validate_parent_path("escape/nested/loot.txt")
        .await
        .expect_err("a real escape must still be refused");
    assert!(
        deep.contains("Resolved parent path escapes workspace"),
        "expected the resolved-parent refusal: {deep}"
    );

    // The same arm, reached instead by a workspace nobody ever created.
    let absent = agent_workspace(root.path(), &CompanyId::new("acme"), "nobody");
    let missing = workspace_security(&absent)
        .validate_parent_path("notes.md")
        .await
        .expect_err("a missing workspace refuses too");
    assert!(
        missing.contains("Resolved parent path escapes workspace"),
        "{missing}"
    );

    // Verbatim identical up to the path each names. A reader given either
    // one goes looking for a traversal attempt; only one of them is.
    let strip = |m: &str| {
        m.split_once("escapes workspace: ")
            .map(|(head, _)| head.to_string())
            .unwrap_or_default()
    };
    assert_eq!(
        strip(&deep),
        strip(&missing),
        "an attack and an unprovisioned directory should not read alike"
    );
}

/// A `..` traversal is refused earlier, by the string-level check, and
/// *does* read differently — so the ambiguity above is specifically about
/// the resolved-parent arm, not about every refusal.
#[tokio::test]
async fn a_dot_dot_traversal_is_refused_with_a_distinguishable_message() {
    let root = tempfile::tempdir().expect("tempdir");
    let workspace =
        ensure_agent_workspace(root.path(), &CompanyId::new("acme"), "ceo").expect("ensure");

    let err = workspace_security(&workspace)
        .validate_parent_path("../../loot.txt")
        .await
        .expect_err("a traversal is refused");
    assert!(
        err.contains("Path not allowed by security policy"),
        "expected the string-level refusal: {err}"
    );
    assert!(
        !err.contains("escapes workspace"),
        "this arm is already distinguishable: {err}"
    );
}

#[test]
fn model_for_tier_maps_hints_and_defaults() {
    assert_eq!(model_for_tier(Some("reasoning")), "reasoning-v1");
    assert_eq!(model_for_tier(Some("AGENTIC")), "agentic-v1");
    assert_eq!(model_for_tier(Some("frontend")), "agentic-v1");
    assert_eq!(model_for_tier(None), "chat-v1");
    assert_eq!(model_for_tier(Some("mystery")), "chat-v1");
}

#[test]
fn persona_frames_role_company_and_description() {
    let agent = manifest_agent("Chief Executive", Some("Sets direction."));
    let persona = persona_prompt("Acme", &agent, None);
    assert!(persona.contains("Chief Executive"), "{persona}");
    assert!(persona.contains("Acme"), "{persona}");
    assert!(persona.contains("first person"), "{persona}");
    assert!(persona.ends_with("Sets direction."), "{persona}");
}

#[test]
fn persona_omits_absent_or_blank_description() {
    let persona = persona_prompt("Acme", &manifest_agent("Engineer", Some("   ")), None);
    assert!(persona.contains("Engineer"));
    assert!(!persona.contains("   Engineer"));
    // No trailing description clause.
    assert!(persona.trim_end().ends_with("role."), "{persona}");
}

/// The brief's native set is read off the wired belt: an explicit `search`
/// grant with a credential wires `web_search`, so `search` shows up in the
/// belt's native capabilities — and a bare `*` (which never wires the metered
/// tool) does not.
#[test]
fn native_capabilities_on_belt_track_the_wired_search_tool() {
    let granted = built_native_caps_with_search(&["search"]);
    assert!(
        granted.contains(&"search".to_string()),
        "an explicit search grant wires web_search, so `search` is native on the belt: {granted:?}"
    );
    let wildcard = built_native_caps_with_search(&["*"]);
    assert!(
        !wildcard.contains(&"search".to_string()),
        "a bare `*` wires no metered search tool, so `search` is not native on the belt: {wildcard:?}"
    );
}

/// A company's own provider REPLACES the managed surface rather than
/// joining it, and still answers to the one name the skills know.
///
/// Both halves matter. Two "search the web" tools on one belt would let the
/// model spend the platform's metered budget for a company that pasted its
/// own key — the exact bill-swap the BYO surface exists to prevent. And a
/// belt where the canonical name changed with the provider would break the
/// shipped research skills, which name `web_search` in their instructions.
#[test]
fn a_company_provider_replaces_the_managed_search_tool_under_the_same_name() {
    let byo = built_tool_names_with_byo_search(&["search"], "brave");

    assert!(
        byo.contains(&"web_search".to_string()),
        "the canonical name must survive the provider switch: {byo:?}"
    );
    assert!(
        byo.contains(&"brave_news_search".to_string()),
        "the provider's own extras must be wired too: {byo:?}"
    );
    // Exactly one tool answers to the canonical name.
    assert_eq!(
        byo.iter().filter(|name| *name == "web_search").count(),
        1,
        "two search tools under one name: {byo:?}"
    );
    // And the managed family's siblings are absent — nothing on this belt
    // reaches the platform's metered backend.
    assert!(
        !byo.contains(&"exa_search".to_string()),
        "a Brave company must not carry Exa tools: {byo:?}"
    );
}

/// The BYO surface rides the SAME explicit grant as the metered one. A
/// company key does not turn `search` into a wildcard-conferred namespace:
/// the queries still leave the building, and which index reads them is a
/// decision the manifest makes by name.
#[test]
fn a_wildcard_grant_confers_no_search_tools_even_with_a_company_provider() {
    let wildcard = built_tool_names_with_byo_search(&["*"], "exa");
    assert!(
        !wildcard.contains(&"web_search".to_string()),
        "{wildcard:?}"
    );
    assert!(
        !wildcard.contains(&"exa_get_contents".to_string()),
        "{wildcard:?}"
    );
}

/// Issue #244's two gates, in one table.
///
/// The fail-closed row is the load-bearing one: an agent granted file tools
/// with **no artifact store** must not be offered `publish_artifact`. The
/// tool stages into a queue; with nothing to drain it, a call would report
/// success, tell the agent its deliverable was safe, and drop it. Not
/// offering it is the only honest option.
#[test]
fn publish_artifact_needs_both_a_file_grant_and_a_store() {
    let tool = crate::harness::publish::PUBLISH_ARTIFACT_TOOL.to_string();

    // `files` (and its aliases and the wildcard) + a store → present.
    // Publishing spends nothing and reaches nothing outside the company, so
    // unlike `media`/`search` it rides the ordinary namespace rule.
    for grant in ["files", "docs", "files.write", "*"] {
        let names = built_tool_names_with_artifacts(&[grant]);
        assert!(
            names.contains(&tool),
            "`{grant}` + a store must wire publish_artifact: {names:?}"
        );
    }

    // No file grant → absent. An agent that cannot write a file has nothing
    // to publish.
    let unfiled = built_tool_names_with_artifacts(&["web"]);
    assert!(
        !unfiled.contains(&tool),
        "an agent with no file tools must not be offered publish_artifact: {unfiled:?}"
    );

    // File grant, NO store → absent, fail-closed.
    let storeless = built_tool_names(&["files"], false);
    assert!(
        !storeless.contains(&tool),
        "without an artifact store the tool would stage into a void: {storeless:?}"
    );
    // …and the rest of the file belt is untouched, so the gate withholds one
    // tool rather than breaking the agent.
    assert!(
        storeless.contains(&"file_write".to_string()),
        "{storeless:?}"
    );
}

/// **Issue #1192, the standard issue #886 stated.** The verdict the console
/// renders must equal what the toolbelt actually wires — asserted by running
/// both over the same grant matrix, not by reading the two implementations
/// and agreeing they look alike.
///
/// The console panel calls
/// [`grants_files_or_docs`](crate::company::grants_files_or_docs); this gate
/// calls it too, so today the equality is true by construction. That is the
/// point of pinning it: the day somebody re-inlines a `starts_with` on
/// either side — or "tidies" the predicate into the `_explicit` family,
/// where `*` confers nothing — this fails instead of a panel quietly
/// reporting a capability no agent has, which is the failure #886 was filed
/// about and the failure #886 said a test like this one prevents.
///
/// An artifact store is wired throughout, so the store gate is held constant
/// and the grant is the only variable — which is exactly the axis the
/// console field answers on. (The store half is not a console field at all:
/// production always configures one, so a `artifactStoreConfigured` flag
/// would serialize a hardcoded `true`.)
#[test]
fn the_capability_verdict_matches_what_the_toolbelt_wires() {
    let tool = crate::harness::publish::PUBLISH_ARTIFACT_TOOL.to_string();
    for grant in [
        "*",
        "files",
        "docs",
        "files.write",
        "docs.read",
        "web",
        "shell",
        "documentation",
        "docsy",
        "filesystem",
        "composio",
        "repo",
    ] {
        let verdict = crate::company::grants_files_or_docs(&[grant.to_string()]);
        let wired = built_tool_names_with_artifacts(&[grant]).contains(&tool);
        assert_eq!(
            verdict, wired,
            "`{grant}`: the console would report publishing={verdict} while the toolbelt \
             wires={wired}"
        );
    }
}
