use super::toolbelt_test_helpers_tests::*;
use super::*;
use std::collections::HashSet;

/// The brief must name every tool the flag it rides on actually wires, or
/// it re-creates the bug it exists to fix one namespace at a time. Each
/// list is read off the matching constructor above rather than retyped, so
/// a belt that grows a tool fails here instead of shipping a brief that
/// silently omits it.
#[test]
fn each_flag_names_exactly_the_tools_its_constructor_wires() {
    let ws = Path::new("/tmp/agent-ws");
    let security = test_security(ws, PolicyMode::Full);

    let shell = sandbox_brief(false, true, false);
    for tool in names(&shell_tools(
        security.clone(),
        native_runtime(),
        Some(ShellAudit::disabled()),
        ws,
    )) {
        assert!(shell.contains(tool), "the shell brief never names `{tool}`");
    }

    let code = sandbox_brief(false, false, true);
    for tool in names(&code_tools(security, ws)) {
        assert!(code.contains(tool), "the code brief never names `{tool}`");
    }

    // `file_tools` lives in `build` (behind the same feature as this
    // module), so its belt is named literally here and pinned by
    // `build::file_tools_are_sandboxed_to_the_workspace` on the other side.
    let files = sandbox_brief(true, false, false);
    for tool in ["file_read", "file_write", "edit", "list", "glob", "grep"] {
        assert!(files.contains(tool), "the file brief never names `{tool}`");
    }
}

/// A brief that describes an ungranted namespace costs a turn per
/// hallucinated call, so each clause must be absent when its flag is.
#[test]
fn a_clause_is_absent_when_its_namespace_is_not_granted() {
    let files_only = sandbox_brief(true, false, false);
    assert!(!files_only.contains("`shell`"), "{files_only}");
    assert!(!files_only.contains("apply_patch"), "{files_only}");

    let shell_only = sandbox_brief(false, true, false);
    assert!(!shell_only.contains("file_write"), "{shell_only}");
    assert!(!shell_only.contains("csv_export"), "{shell_only}");
}

/// An agent holding none of the three gets no section at all — not an empty
/// heading, which would read as a surface it has and cannot find.
#[test]
fn an_agent_with_no_sandbox_namespace_gets_no_section() {
    assert_eq!(sandbox_brief(false, false, false), "");
}

#[test]
fn web_brief_distinguishes_fetching_from_discovery() {
    let fetch_only = web_brief(true, false);
    assert!(fetch_only.contains("Use `web_fetch`"));
    assert!(fetch_only.contains("No `web_search` provider is connected"));
    assert!(fetch_only.contains("Do not substitute repeated workspace or ledger reads"));

    let with_search = web_brief(true, true);
    assert!(with_search.contains("Use `web_search` to discover"));
    assert!(!with_search.contains("No `web_search` provider is connected"));
    assert!(with_search.contains("stop after that one call"));

    let search_only = web_brief(false, true);
    assert!(search_only.contains("URL fetching is not granted"));
    assert!(!search_only.contains("with `web_fetch`"));

    assert_eq!(web_brief(false, false), "");
}

/// The two things the sandbox brief exists to say, both of which the belt
/// enforces whether or not the agent knows them: file/code paths are
/// confined (`exec_security` sets `workspace_only`), and producing the
/// thing means writing it rather than recording a task about it.
#[test]
fn the_brief_states_the_confinement_and_the_write_it_instruction() {
    let brief = sandbox_brief(true, true, true);
    assert!(
        brief.contains("../"),
        "the escape rule must be shown: {brief}"
    );
    assert!(
        brief.contains("actually write the file"),
        "the instruction that motivates this brief is missing: {brief}"
    );
    assert!(
        brief.contains("Recording a task about the work"),
        "the observed failure must be named: {brief}"
    );
}

/// The confinement claim must be scoped to the tools that enforce it.
/// `workspace_only` refuses an absolute path or a `../` escape for the
/// file/code tools, but `action_dir` only sets the shell's *working
/// directory* — a same-uid command can read anywhere the server can
/// (docs/spec/security/agent-isolation.md). So the shell clause must
/// describe the directory as where commands start, never as a jail.
#[test]
fn the_shell_clause_does_not_claim_confinement() {
    let shell_only = sandbox_brief(false, true, false);
    assert!(!shell_only.contains("cannot leave"), "{shell_only}");
    assert!(!shell_only.contains("nothing outside"), "{shell_only}");
    assert!(
        shell_only.contains("starts in that same directory"),
        "{shell_only}"
    );

    // The refusal sentence stays with the file tools that enforce it.
    let files_only = sandbox_brief(true, false, false);
    assert!(
        files_only.contains("`../` escape is refused"),
        "{files_only}"
    );
}

/// "Run the command" is a command-running instruction, and the only tool
/// that runs arbitrary commands is `shell`. A belt without `shell` must
/// not be told to run anything — that re-creates the unavailable-tool
/// prompt mismatch the namespace filtering exists to prevent.
#[test]
fn the_run_instruction_is_gated_on_shell() {
    let files_only = sandbox_brief(true, false, false);
    assert!(!files_only.contains("run the command"), "{files_only}");
    assert!(!files_only.contains("or run"), "{files_only}");

    let with_shell = sandbox_brief(true, true, false);
    assert!(with_shell.contains("run the command"), "{with_shell}");
}

#[test]
fn shell_tools_expose_expected_names() {
    let ws = Path::new("/tmp/oc-toolbelt-shell");
    let security = test_security(ws, PolicyMode::Supervised);
    let tools = shell_tools(security, native_runtime(), Some(ShellAudit::disabled()), ws);
    let got = names(&tools);
    for expected in ["shell", "read_workspace_state"] {
        assert!(got.contains(&expected), "missing {expected}: {got:?}");
    }
    assert_eq!(got.len(), 2, "shell tools drifted: {got:?}");
}

/// Fail-closed guard: when the workspace audit logger cannot be built
/// (`workspace_audit` → `None`), `shell_tools` MUST withhold the entire
/// `shell` namespace rather than register a `ShellTool` that would execute
/// commands with no audit record. This is the security boundary — pin it.
#[test]
fn shell_tools_absent_when_audit_init_fails() {
    let ws = Path::new("/tmp/oc-toolbelt-shell-noaudit");
    let security = test_security(ws, PolicyMode::Supervised);
    let tools = shell_tools(security, native_runtime(), None, ws);
    assert!(
        tools.is_empty(),
        "shell namespace must be withheld when audit init fails, got: {:?}",
        names(&tools)
    );
}

#[test]
fn code_tools_expose_expected_names() {
    let ws = Path::new("/tmp/oc-toolbelt-code");
    let security = test_security(ws, PolicyMode::Supervised);
    let tools = code_tools(security, ws);
    let got = names(&tools);
    for expected in ["apply_patch", "git_operations", "csv_export"] {
        assert!(got.contains(&expected), "missing {expected}: {got:?}");
    }
    assert_eq!(got.len(), 3, "code tools drifted: {got:?}");
}

/// The `shell` and `code` grant namespaces must build from **disjoint** tool
/// vectors: granting `code` alone must never hand an agent a live `ShellTool`
/// (and vice versa). The production `CapabilityFilter` is identity, so this
/// tool-vector split is the only thing enforcing the boundary — pin it.
#[test]
fn shell_and_code_tool_sets_are_disjoint_and_correctly_namespaced() {
    let ws = Path::new("/tmp/oc-toolbelt-isolation");
    let security = test_security(ws, PolicyMode::Supervised);

    let shell = shell_tools(
        security.clone(),
        native_runtime(),
        Some(ShellAudit::disabled()),
        ws,
    );
    let code = code_tools(security, ws);

    // Every shell tool maps to the `shell` namespace and none to `code`.
    for tool in &shell {
        assert_eq!(
            namespace_of(tool.name()),
            Some("shell"),
            "shell_tools leaked a non-shell tool: {}",
            tool.name()
        );
    }
    // Every code tool maps to the `code` namespace and none to `shell`.
    for tool in &code {
        assert_eq!(
            namespace_of(tool.name()),
            Some("code"),
            "code_tools leaked a non-code tool: {}",
            tool.name()
        );
    }

    // No tool name appears in both vectors.
    let shell_names: HashSet<&str> = names(&shell).into_iter().collect();
    let code_names: HashSet<&str> = names(&code).into_iter().collect();
    assert!(
        shell_names.is_disjoint(&code_names),
        "shell/code tool sets overlap: {shell_names:?} ∩ {code_names:?}"
    );
}

#[test]
fn web_tools_expose_expected_names() {
    let ws = Path::new("/tmp/oc-toolbelt-web");
    let security = test_security(ws, PolicyMode::Supervised);
    // Empty allowlist = open-public mode; the SSRF IP guard still applies.
    let tools = web_tools(security, Vec::new(), ws);
    let got = names(&tools);
    for expected in ["web_fetch", "http_request", "curl", "image_info"] {
        assert!(got.contains(&expected), "missing {expected}: {got:?}");
    }
    assert_eq!(got.len(), 4, "web tools drifted: {got:?}");
}

#[test]
fn subagent_tools_are_reserved_empty() {
    assert!(
        subagent_tools().is_empty(),
        "subagent namespace is v1-reserved"
    );
}

#[test]
fn namespace_table_maps_exec_tools_and_leaves_intrinsic_unmapped() {
    assert_eq!(namespace_of("shell"), Some("shell"));
    assert_eq!(namespace_of("read_workspace_state"), Some("shell"));
    assert_eq!(namespace_of("apply_patch"), Some("code"));
    assert_eq!(namespace_of("git_operations"), Some("code"));
    assert_eq!(namespace_of("csv_export"), Some("code"));
    assert_eq!(namespace_of("web_fetch"), Some("web"));
    assert_eq!(namespace_of("http_request"), Some("web"));
    assert_eq!(namespace_of("curl"), Some("web"));
    assert_eq!(namespace_of("image_info"), Some("web"));
    // Media generation (issue #109) maps to the `media` namespace.
    assert_eq!(namespace_of("media_generate_image"), Some("media"));
    assert_eq!(namespace_of("media_generate_video"), Some("media"));
    assert_eq!(namespace_of("media_list_models"), Some("media"));
    // Per-tenant Composio (issue #110) maps to the `composio` namespace.
    assert_eq!(namespace_of("composio_list_toolkits"), Some("composio"));
    assert_eq!(namespace_of("composio_list_connections"), Some("composio"));
    assert_eq!(namespace_of("composio_list_tools"), Some("composio"));
    assert_eq!(namespace_of("composio_authorize"), Some("composio"));
    assert_eq!(namespace_of("composio_execute"), Some("composio"));
    // Metered web search (issue #238) maps to the `search` namespace, so a
    // token-budget plan can shed it under spend pressure.
    assert_eq!(namespace_of("web_search"), Some("search"));
    // Intrinsic tools are unmapped (always kept by the filter).
    assert_eq!(namespace_of("memory_store"), None);
    assert_eq!(namespace_of("memory_recall"), None);
    assert_eq!(namespace_of("memory_forget"), None);
    assert_eq!(namespace_of("file_read"), None);
    assert_eq!(namespace_of("mcp_registry_tool_call"), None);
}

/// The `url`-taking web subset the S2 deflection guardrail keys on: the three
/// raw HTTP tools, and NOT `image_info` (which is `web` but reads a workspace
/// file, not a URL) nor anything outside the family.
#[test]
fn is_web_request_tool_is_the_url_taking_web_subset() {
    assert!(is_web_request_tool("web_fetch"));
    assert!(is_web_request_tool("http_request"));
    assert!(is_web_request_tool("curl"));
    assert!(!is_web_request_tool("image_info"));
    assert!(!is_web_request_tool("shell"));
    assert!(!is_web_request_tool("composio_execute"));
}

/// `GATEABLE_NAMESPACES` must be a superset of every namespace `namespace_of`
/// can emit — otherwise an exec family would be ungateable (silently always
/// granted). `subagent` is additionally present as the reserved namespace.
#[test]
fn gateable_namespaces_cover_every_mapped_namespace() {
    let mapped = [
        "shell",
        "read_workspace_state",
        "apply_patch",
        "git_operations",
        "csv_export",
        "web_fetch",
        "http_request",
        "curl",
        "image_info",
        "media_generate_image",
        "media_generate_video",
        "media_list_models",
        "composio_list_toolkits",
        "composio_list_connections",
        "composio_list_tools",
        "composio_authorize",
        "composio_execute",
        "web_search",
        // The BYO search extras (issue #238 follow-up). Listed by name
        // rather than spliced in from `BYO_SEARCH_TOOLS` so this test keeps
        // saying what it checks: every tool a belt can carry is mapped onto
        // a gateable namespace.
        "exa_find_similar",
        "exa_get_contents",
        "brave_news_search",
        "brave_image_search",
        "brave_video_search",
    ];
    for tool in mapped {
        let ns = namespace_of(tool).expect("mapped tool has a namespace");
        assert!(
            GATEABLE_NAMESPACES.contains(&ns),
            "namespace `{ns}` (from `{tool}`) is not gateable"
        );
    }
    assert!(
        GATEABLE_NAMESPACES.contains(&"subagent"),
        "the reserved subagent namespace must be gateable"
    );
    assert!(
        GATEABLE_NAMESPACES.contains(&"media"),
        "the real-money media namespace must be gateable"
    );
    assert!(
        GATEABLE_NAMESPACES.contains(&"composio"),
        "the per-tenant composio namespace must be gateable"
    );
    assert!(
        GATEABLE_NAMESPACES.contains(&"search"),
        "the metered search namespace must be gateable (issue #238)"
    );
}

/// Every namespace `namespace_of` can emit that is neither the Composio
/// connection path nor the raw-HTTP `web` family must be in the shared
/// native vocabulary — otherwise a future native tool would be wired but
/// invisible to native-first routing (the brief and the classifier both key
/// off that vocabulary).
#[test]
fn native_vocabulary_covers_every_native_mapped_namespace() {
    let mapped = [
        "shell",
        "read_workspace_state",
        "apply_patch",
        "git_operations",
        "csv_export",
        "web_fetch",
        "http_request",
        "curl",
        "image_info",
        "media_generate_image",
        "media_generate_video",
        "media_list_models",
        "composio_list_toolkits",
        "composio_list_connections",
        "composio_list_tools",
        "composio_authorize",
        "composio_execute",
        "web_search",
        "exa_find_similar",
        "exa_get_contents",
        "brave_news_search",
        "brave_image_search",
        "brave_video_search",
    ];
    let native: std::collections::HashSet<&str> = crate::company::native_capability_namespaces()
        .into_iter()
        .collect();
    for tool in mapped {
        let ns = namespace_of(tool).expect("mapped tool has a namespace");
        if ns == "composio" || ns == "web" {
            continue;
        }
        assert!(
            native.contains(ns),
            "native namespace `{ns}` (from `{tool}`) is not in the native vocabulary"
        );
    }
}
