use super::consequence_hosting_tests::*;
use super::*;
use serde_json::json;

// -----------------------------------------------------------------------
// Issue #875: `shell`, classified by the command it was handed
// -----------------------------------------------------------------------

// Gated to match its callers. Every test below that grades a shell command
// is `#[cfg(feature = "openhuman")]`, so without the feature they compile
// away and this helper is left with none — `dead_code` under the default
// lane's `-D warnings`, which is what turned the `Rust` job red.
#[cfg(feature = "openhuman")]
pub(super) fn shell(command: &str) -> Consequence {
    consequence_of(SHELL, &json!({ SHELL_COMMAND_KEY: command }))
}

/// The complaint this issue is about: an agent looking at its own workspace
/// paid an approval per command. These are the exact shapes an operator was
/// approving on staging.
#[test]
#[cfg(feature = "openhuman")]
pub(super) fn a_read_of_the_agents_own_workspace_runs_unattended() {
    for command in [
        "grep -l -i \"resets\\|forgot\" session_raw/*.jsonl",
        "grep -c -i plus session_raw/*.jsonl",
        "find . -maxdepth 4 -type d",
        "cat notes.md",
        "ls -la",
        "wc -l src/main.rs",
    ] {
        let c = shell(command);
        assert_eq!(
            c.reach,
            Reach::Nothing,
            "`{command}` reads and changes nothing"
        );
        assert!(
            !c.reach.parks_under_supervision(),
            "`{command}` must not park under any acting tier"
        );
    }
}

/// Bases omitted from the vendor's name-only allowlist may run unattended
/// only when their actual argv excludes every writing form we admit around.
#[test]
#[cfg(feature = "openhuman")]
pub(super) fn argv_sensitive_workspace_reads_run_without_admitting_writes() {
    for command in [
        "sed -n '860,915p' src/policy/consequence.rs",
        "sed 's/old/new/g' notes.txt",
        "sort names.txt",
        "sort -r names.txt",
        "awk '{ print $1 }' data.txt",
        "awk -F, '{ print $2 }' data.csv",
    ] {
        assert_eq!(
            shell(command).reach,
            Reach::Nothing,
            "`{command}` has a provably read-only argv"
        );
    }

    for command in [
        "sed -i 's/old/new/g' notes.txt",
        "sed -ni.bak 's/old/new/g' notes.txt",
        "sed --in-place=.bak 's/old/new/g' notes.txt",
        "sed -f transform.sed notes.txt",
        "sort -o sorted.txt names.txt",
        "sort -ruooutput.txt names.txt",
        "sort --output=sorted.txt names.txt",
        "sort --compress-program=gzip names.txt",
        "awk '{ print $1 > \"out.txt\" }' data.txt",
        "awk '{ print $1 }' data.txt > out.txt",
        "awk '{ print $1 | \"tee out.txt\" }' data.txt",
        "awk 'BEGIN { system(\"touch out.txt\") }' data.txt",
        "awk -f report.awk data.txt",
    ] {
        assert_eq!(
            shell(command).reach,
            Reach::Consequence,
            "`{command}` can write or execute and must park"
        );
    }
}

/// The argv exceptions remain behind #876's lexical containment boundary.
#[test]
#[cfg(feature = "openhuman")]
pub(super) fn argv_sensitive_reads_still_refuse_escapes_and_ambiguous_shell_syntax() {
    for command in [
        "sed -n '1p' /etc/passwd",
        "sort /etc/passwd",
        "awk '{ print $1 }' /etc/passwd",
        "sed -n '1p' ~/.ssh/config",
        "sort ~/secrets.txt",
        "awk '{ print $1 }' ~/.secrets",
        "sed -n '1p' ../secret.txt",
        "sort data/../../secret.txt",
        "awk '{ print $1 }' ../secret.txt",
        "sed -n \"$(cat program.sed)\" notes.txt",
        "sort \"$(cat filenames.txt)\"",
        "awk \"$(cat program.awk)\" data.txt",
        "sed -n '1p notes.txt",
        "sort $SORT_FLAGS names.txt",
        "awk '{ print $1 }' data.txt; rm data.txt",
    ] {
        assert_eq!(
            shell(command).reach,
            Reach::Consequence,
            "`{command}` is outside the lexical boundary or has ambiguous argv"
        );
    }
}

/// A command the vendored classifier grades `Read` — because it grades by
/// command name, never by path — must still park when its arguments name a
/// location outside the agent's own directory. Falsified against the
/// pre-fix behaviour: before `shell_command_reaches_outside_cwd` existed,
/// `shell_command_is_read` alone was sufficient and every one of these
/// downgraded to `Reach::Nothing` — `cat`/`ls`/`grep`/`readlink` are all in
/// the vendored `READ_ONLY_BASES` regardless of what they are pointed at.
#[test]
#[cfg(feature = "openhuman")]
pub(super) fn a_read_that_reaches_outside_the_workspace_still_parks() {
    for command in [
        "cat /etc/passwd",
        "cat ~/.ssh/id_rsa",
        "ls /root",
        "grep -r secret /etc",
        "readlink ~",
        "cat ../../secrets.env",
        "head --lines=5 /var/log/auth.log",
        "cat notes/../../../etc/passwd",
    ] {
        let c = shell(command);
        assert_eq!(
            c.reach,
            Reach::Consequence,
            "`{command}` reaches outside the workspace and must still park"
        );
        assert!(
            c.parks_under_auto(),
            "`{command}` must still park under auto"
        );
    }
}

/// The lexical backstop alone, independent of the classifier — pins the
/// exact set of shapes it does and does not flag. No `openhuman` feature
/// needed: this is pure string logic with no classifier dependency.
#[test]
pub(super) fn shell_command_reaches_outside_cwd_flags_the_realistic_escapes() {
    for command in [
        "cat /etc/passwd",
        "ls ~/.ssh",
        "cat ../secret.env",
        "cat notes/../../etc/passwd",
        "head --lines=5 /var/log/auth.log",
        "cat \"/etc/passwd\"",
    ] {
        assert!(
            shell_command_reaches_outside_cwd(command),
            "`{command}` should be flagged as reaching outside the cwd"
        );
    }

    for command in [
        "cat notes.md",
        "grep -l foo session_raw/*.jsonl",
        "find . -maxdepth 4 -type d",
        "ls -la",
        "wc -l src/main.rs",
    ] {
        assert!(
            !shell_command_reaches_outside_cwd(command),
            "`{command}` stays inside the cwd and should not be flagged"
        );
    }
}

/// Everything that is not provably a read keeps exactly the verdict it had
/// before this issue: it parks, and it can hold no standing grant.
#[test]
#[cfg(feature = "openhuman")]
pub(super) fn anything_that_acts_still_parks() {
    for command in [
        "rm -rf /",
        "curl https://example.com",
        "npm install -g something",
        "echo hi > file.txt",
        "git push origin main",
        "chmod 777 /etc/passwd",
    ] {
        let c = shell(command);
        assert_eq!(c.reach, Reach::Consequence, "`{command}` acts");
        assert_eq!(
            c.standing,
            Standing::PerCall,
            "`{command}` may hold no standing grant"
        );
        assert!(
            c.parks_under_auto(),
            "`{command}` must still park under auto"
        );
    }
}

// ── git_operations, graded by its `operation` (issue #877) ─────────────

pub(super) fn git(operation: &str) -> Consequence {
    consequence_of(GIT_OPERATIONS, &json!({ GIT_OPERATION_KEY: operation }))
}

/// Orienting in your own workspace should not cost an operator anything.
#[test]
pub(super) fn a_git_read_operation_does_not_park() {
    for operation in GIT_READ_ONLY_OPERATIONS {
        let c = git(operation);
        assert_eq!(
            c.reach,
            Reach::Nothing,
            "`git {operation}` only reads the repository"
        );
        assert!(
            !c.parks_under_auto(),
            "`git {operation}` must not interrupt anybody"
        );
    }
}

/// The writes upstream names still park. Without this the downgrade above
/// would pass against a build that stopped gating everything.
#[test]
pub(super) fn a_git_write_operation_still_parks() {
    for operation in ["commit", "add", "checkout", "stash", "reset", "revert"] {
        let c = git(operation);
        assert_eq!(c.reach, Reach::Consequence, "`git {operation}` acts");
        assert!(
            c.parks_under_auto(),
            "`git {operation}` must still park under auto"
        );
    }
}

/// **The fail-closed requirement.** An operation this classifier does not
/// recognise must still ask.
///
/// The first six are real git subcommands in **neither** upstream list —
/// `requires_write_access` does not name them and `is_read_only` does not
/// either — so they are genuinely unclassified rather than merely absent
/// from a list somebody forgot to extend. `push` is the one that matters
/// most: it reaches a configured remote, which is an address this layer
/// never sees. The last two are a typo and an invented name, which is what
/// a model produces on a bad day.
///
/// This passing is the whole safety argument for the downgrade: membership
/// is affirmative, so the failure mode of an unknown operation is an extra
/// approval, never a silent act.
#[test]
pub(super) fn an_unrecognised_git_operation_still_parks() {
    for operation in [
        "push",
        "pull",
        "fetch",
        "merge",
        "rebase",
        "clone",
        "stauts",
        "frobnicate",
    ] {
        let c = git(operation);
        assert_eq!(
            c.reach,
            Reach::Consequence,
            "`git {operation}` is not provably a read, so it must ask"
        );
        assert!(
            c.parks_under_auto(),
            "`git {operation}` must park under auto"
        );
    }
}

/// An argument that cannot be read gates. The tool's schema marks
/// `operation` required, so each of these is a call that could not have run
/// — guessing at one would be inventing a verdict for a call that never
/// happened.
#[test]
pub(super) fn a_git_call_with_no_readable_operation_parks() {
    for args in [
        json!({}),
        json!({ GIT_OPERATION_KEY: null }),
        json!({ GIT_OPERATION_KEY: 7 }),
        json!({ GIT_OPERATION_KEY: ["status"] }),
        json!({ "op": "status" }),
    ] {
        let c = consequence_of(GIT_OPERATIONS, &args);
        assert_eq!(
            c.reach,
            Reach::Consequence,
            "unreadable args must park: {args}"
        );
    }
}

/// Case matters, matching upstream's `matches!`. `STATUS` is not `status`,
/// and a classifier that normalised case here would be answering a question
/// upstream does not ask.
#[test]
pub(super) fn git_operation_matching_is_case_sensitive() {
    for operation in ["STATUS", "Status", "LOG"] {
        assert_eq!(
            git(operation).reach,
            Reach::Consequence,
            "`{operation}` is not the operation upstream classifies"
        );
    }
}

/// **The oracle.** [`GIT_READ_ONLY_OPERATIONS`] is a copy of a vendored
/// list, and a copy that can drift silently is exactly what issue #877
/// warns against. This drives the vendored judgement directly, so upstream
/// reclassifying any of these fails the build here rather than quietly
/// widening what runs unattended.
///
/// It asserts the safety-relevant direction: **none of the operations this
/// crate downgrades is a write upstream**. The converse is not assertable —
/// `is_read_only` is a private inherent method — but it is also not the
/// dangerous direction: an operation upstream calls read-only that we
/// nonetheless gate costs an approval, while the reverse would run a write
/// unattended.
///
/// `SecurityPolicy::default()` is `AutonomyLevel::Supervised`, where
/// `gate_decision(Write)` is `Prompt` — so the tier half of
/// `external_effect_with_args`'s conjunction is `true` and the expression
/// reduces to `requires_write_access(operation)` alone. That is the only
/// configuration in which this hook answers the question this crate is
/// asking, which is why the gate itself must not call it (see
/// [`git_operations_consequence`]).
#[test]
#[cfg(feature = "openhuman")]
pub(super) fn the_read_only_set_matches_the_vendored_classifier() {
    use openhuman_core::security::SecurityPolicy;
    use openhuman_core::tools::GitOperationsTool;
    use tinytools::Tool;

    let policy = std::sync::Arc::new(SecurityPolicy::default());
    let tool = GitOperationsTool::new(policy, std::path::PathBuf::from("."));

    for operation in GIT_READ_ONLY_OPERATIONS {
        assert!(
            !tool.external_effect_with_args(&json!({ GIT_OPERATION_KEY: operation })),
            "upstream now treats `git {operation}` as a write — this crate is downgrading \
             something that acts. Remove it from GIT_READ_ONLY_OPERATIONS."
        );
    }

    // And the pairing that proves the oracle is live rather than vacuous: a
    // known write must come back `true` through the same call.
    assert!(
        tool.external_effect_with_args(&json!({ GIT_OPERATION_KEY: "commit" })),
        "the oracle answered `false` for a commit, so it is not testing anything"
    );
}

/// The classifier takes the maximum across segments, so a read cannot carry
/// an act through on its coat-tails. This is the property that makes
/// downgrading reads safe at all.
#[test]
#[cfg(feature = "openhuman")]
pub(super) fn a_read_chained_to_an_act_is_an_act() {
    for command in [
        "grep -r foo . && rm -rf /tmp/x",
        "ls; curl https://example.com",
        "cat a.txt | tee b.txt",
        "find . -type f > listing.txt",
    ] {
        assert_eq!(
            shell(command).reach,
            Reach::Consequence,
            "`{command}` contains an act and must park"
        );
    }
}

/// The model's own label may raise the requirement and never lower it.
#[test]
#[cfg(feature = "openhuman")]
pub(super) fn a_declared_category_escalates_only() {
    // A read the model calls destructive parks…
    let escalated = consequence_of(
        SHELL,
        &json!({ SHELL_COMMAND_KEY: "ls -la", SHELL_CATEGORY_KEY: "destructive" }),
    );
    assert_eq!(escalated.reach, Reach::Consequence);

    // …and an act the model calls a read does not stop parking.
    let attempted_downgrade = consequence_of(
        SHELL,
        &json!({ SHELL_COMMAND_KEY: "rm -rf /", SHELL_CATEGORY_KEY: "read" }),
    );
    assert_eq!(attempted_downgrade.reach, Reach::Consequence);
}

/// A call this cannot read is gated. The tool's schema requires `command`,
/// so every one of these is a call that could not have run — and none of
/// them is a reason to guess.
#[test]
pub(super) fn an_unreadable_shell_call_is_gated() {
    for args in [
        json!({}),
        json!({ SHELL_COMMAND_KEY: 7 }),
        json!({ SHELL_COMMAND_KEY: null }),
        json!(null),
        json!("ls"),
    ] {
        let c = consequence_of(SHELL, &args);
        assert_eq!(c.reach, Reach::Consequence, "{args}");
        assert!(c.parks_under_auto(), "{args}");
    }
}

/// The name-level declaration is untouched: every reader that asks about
/// `shell` without arguments — the permissions list, the console labels,
/// the coverage test — still sees the gated answer.
#[test]
pub(super) fn the_declaration_still_reads_as_gated_without_arguments() {
    assert_eq!(c(SHELL).reach, Reach::Consequence);
}

/// Without the harness feature there is no classifier, and the fallback
/// answers "act" for everything. Nothing pinned that: the gated-call test
/// above passes only malformed arguments, which return before
/// `shell_command_is_read` is ever reached, so the fallback could regress to
/// permissive and every default-feature lane would stay green. A command
/// that IS a read under the classifier is the case that separates them.
#[test]
#[cfg(not(feature = "openhuman"))]
pub(super) fn a_read_command_still_parks_when_no_classifier_is_linked_in() {
    let c = consequence_of(SHELL, &json!({ SHELL_COMMAND_KEY: "ls -la" }));
    assert_eq!(c.reach, Reach::Consequence);
    assert!(c.parks_under_auto());
}

// ── MCP bridge calls, graded against a per-server read declaration (#1124) ──

/// A `mcp_call_tool` call as the policy layer sees it.
pub(super) fn mcp_call(server: &str, tool: &str) -> serde_json::Value {
    json!({
        MCP_CALL_SERVER_KEY: server,
        MCP_CALL_TOOL_KEY: tool,
        "arguments": {},
    })
}

/// A `mcp_registry_tool_call` call — different argument keys, same shape.
pub(super) fn registry_call(server_id: &str, tool_name: &str) -> serde_json::Value {
    json!({
        MCP_REGISTRY_SERVER_KEY: server_id,
        MCP_REGISTRY_TOOL_KEY: tool_name,
        "arguments": {},
    })
}

/// **Acceptance criterion 1, for both tools.** A call to a server-declared
/// read-only remote tool does not park under `auto`; every other combination
/// still parks.
///
/// This is the classifier's OWN test (criterion 4): reverting
/// [`mcp_call_reach`] to return its base for the declared pair — the whole of
/// the downgrade — makes the first two assertions fail, because the declared
/// read would park again.
#[test]
pub(super) fn a_declared_read_only_remote_tool_does_not_park_but_everything_else_does() {
    let reads = McpReadSet::from_pairs([
        ("jira".to_string(), "get_issue".to_string()),
        ("registry-42".to_string(), "list_rows".to_string()),
    ]);

    // The declared read on each tool downgrades and stops parking.
    let call_read = mcp_call_reach(MCP_CALL_TOOL, &mcp_call("jira", "get_issue"), &reads);
    assert_eq!(call_read.reach, Reach::ExternalRead);
    assert!(
        !call_read.parks_under_auto(),
        "a server-declared read must not park under auto"
    );
    let registry_read = mcp_call_reach(
        MCP_REGISTRY_TOOL_CALL,
        &registry_call("registry-42", "list_rows"),
        &reads,
    );
    assert_eq!(registry_read.reach, Reach::ExternalRead);
    assert!(!registry_read.parks_under_auto());

    // Every other combination still parks: a write on the same declared
    // server, a read on an undeclared server, and the same declared tool name
    // on the WRONG tool of the pair (server declared, tool not).
    for (tool, args) in [
        (MCP_CALL_TOOL, mcp_call("jira", "create_issue")),
        (MCP_CALL_TOOL, mcp_call("confluence", "get_issue")),
        (MCP_CALL_TOOL, mcp_call("jira", "list_rows")),
        (
            MCP_REGISTRY_TOOL_CALL,
            registry_call("registry-42", "write_row"),
        ),
        (
            MCP_REGISTRY_TOOL_CALL,
            registry_call("registry-99", "list_rows"),
        ),
        // The keys are not interchangeable across the two tools: a
        // registry-shaped payload under `mcp_call_tool` reads no `server`.
        (MCP_CALL_TOOL, registry_call("jira", "get_issue")),
    ] {
        let verdict = mcp_call_reach(tool, &args, &reads);
        assert_eq!(
            verdict.reach,
            Reach::Consequence,
            "`{tool}` {args} is not an affirmatively-declared read and must park"
        );
        assert!(
            verdict.parks_under_auto(),
            "`{tool}` {args} must park under auto"
        );
    }
}

/// The fail-closed base: with no declaration, every bridge call parks — the
/// verdict both tools carried before this issue, and the answer for every
/// non-harness construction site whose policy sets no read declaration.
#[test]
pub(super) fn with_no_declaration_every_bridge_call_gates() {
    let empty = McpReadSet::default();
    assert!(empty.is_empty());
    for (tool, args) in [
        (MCP_CALL_TOOL, mcp_call("jira", "get_issue")),
        (MCP_REGISTRY_TOOL_CALL, registry_call("r", "get_issue")),
    ] {
        let verdict = mcp_call_reach(tool, &args, &empty);
        assert_eq!(verdict.reach, Reach::Consequence);
        assert_eq!(verdict.standing, Standing::PerCall);
        assert!(verdict.parks_under_auto());
    }
}

/// A downgraded read is `ExternalRead`, not `Nothing`: it reaches a third
/// party's server with the company's credential, so a `readonly` desk still
/// denies it and it is never billed — the Composio-read precedent (#559).
#[test]
pub(super) fn a_downgraded_read_is_denied_under_readonly_and_is_not_a_spend() {
    let reads = McpReadSet::from_pairs([("jira".to_string(), "get_issue".to_string())]);
    let verdict = mcp_call_reach(MCP_CALL_TOOL, &mcp_call("jira", "get_issue"), &reads);
    assert_eq!(verdict.reach, Reach::ExternalRead);
    assert!(
        verdict.reach.denied_under_readonly(),
        "a read of a counterparty's account is exactly what readonly refuses"
    );
    assert!(!verdict.reach.costs_money(), "a read is not billed");
    assert!(
        !verdict.reach.parks_under_supervision(),
        "supervised runs it — nothing changes and nothing is spent"
    );
    assert_eq!(verdict.standing, Standing::PerCall);
}

/// A call this cannot read gates, whichever key is missing or mistyped. The
/// tools' schemas mark both required, so each of these is a call that could
/// not have run — the same fail-closed rule the other argument graders keep.
#[test]
pub(super) fn an_unreadable_bridge_call_gates_even_with_a_matching_declaration() {
    let reads = McpReadSet::from_pairs([
        ("jira".to_string(), "get_issue".to_string()),
        ("r".to_string(), "get_issue".to_string()),
    ]);
    let unreadable_call = [
        json!({ MCP_CALL_TOOL_KEY: "get_issue", "arguments": {} }), // no server
        json!({ MCP_CALL_SERVER_KEY: "jira", "arguments": {} }),    // no tool
        json!({ MCP_CALL_SERVER_KEY: 7, MCP_CALL_TOOL_KEY: "get_issue" }), // non-string
        json!({ MCP_CALL_SERVER_KEY: "jira", MCP_CALL_TOOL_KEY: null }),
        json!(null),
        json!("jira"),
    ];
    for args in unreadable_call {
        let verdict = mcp_call_reach(MCP_CALL_TOOL, &args, &reads);
        assert_eq!(verdict.reach, Reach::Consequence, "unreadable: {args}");
        assert!(verdict.parks_under_auto(), "unreadable: {args}");
    }
    // …and the registry twin, under its own keys.
    for args in [
        json!({ MCP_REGISTRY_TOOL_KEY: "get_issue", "arguments": {} }),
        json!({ MCP_REGISTRY_SERVER_KEY: "r", "arguments": {} }),
        json!({ MCP_REGISTRY_SERVER_KEY: "r", MCP_REGISTRY_TOOL_KEY: 7 }),
    ] {
        let verdict = mcp_call_reach(MCP_REGISTRY_TOOL_CALL, &args, &reads);
        assert_eq!(
            verdict.reach,
            Reach::Consequence,
            "unreadable registry: {args}"
        );
    }
}

/// The tool name is matched case-insensitively, the way every other arm of
/// the gate reads it — the argument keys, and the bridge-tool predicate.
#[test]
pub(super) fn the_bridge_tool_name_is_matched_case_insensitively() {
    let reads = McpReadSet::from_pairs([("jira".to_string(), "get_issue".to_string())]);
    assert!(is_mcp_bridge_tool("MCP_CALL_TOOL"));
    assert!(is_mcp_bridge_tool("Mcp_Registry_Tool_Call"));
    assert!(!is_mcp_bridge_tool("mcp_list_tools"));
    let verdict = mcp_call_reach("MCP_CALL_TOOL", &mcp_call("jira", "get_issue"), &reads);
    assert_eq!(verdict.reach, Reach::ExternalRead);
}

/// The plain `consequence_of` — which the roster, the coverage test and every
/// company-blind caller read — still sees the gated verdict for both bridge
/// tools. The downgrade lives only where the declaration does, on the policy.
#[test]
pub(super) fn consequence_of_reads_both_bridge_tools_as_gated() {
    for tool in [MCP_CALL_TOOL, MCP_REGISTRY_TOOL_CALL] {
        let verdict = consequence_of(tool, &mcp_call("jira", "get_issue"));
        assert_eq!(verdict.reach, Reach::Consequence, "`{tool}`");
        assert_eq!(verdict.standing, Standing::PerCall, "`{tool}`");
        assert!(verdict.parks_under_auto(), "`{tool}`");
    }
}

/// **Acceptance criterion 3.** Both bridge tools sit on the argument-graded
/// side of the partition, so the roster and the table stay disjoint and
/// `declared_tools` enumerates each exactly once. This is a direct probe of
/// the same facts `the_roster_and_the_table_partition_the_known_tool_names`
/// enforces over the whole set, named here so a reader of this issue's change
/// sees the criterion asserted.
#[test]
pub(super) fn both_bridge_tools_are_argument_graded_and_enumerated_once() {
    for tool in [MCP_CALL_TOOL, MCP_REGISTRY_TOOL_CALL] {
        assert!(
            argument_grader(tool).is_some(),
            "`{tool}` must be dispatched from its arguments"
        );
        assert_eq!(
            declared_tools().filter(|name| *name == tool).count(),
            1,
            "`{tool}` holds both a roster entry and a DECLARED row and must be enumerated once"
        );
    }
}
