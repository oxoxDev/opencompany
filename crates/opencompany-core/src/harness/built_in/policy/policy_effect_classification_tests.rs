use super::*;
use crate::ports::workspace::{NodeKind, WorkspaceNode, WorkspaceOrigin, WorkspaceStore};
use crate::store::FsOps;

// Issue #470: the `composio_execute` fixtures are built here, from the same
// key the classifier reads, so a call in a test reaches the same catalogue
// lookup a call in production does.
use super::policy_test_helpers_tests::*;

/// Paid generation classifies as a spend effect (issue #109).
#[test]
fn media_generate_classifies_as_spend() {
    let p = policy("supervised", &[], None);
    assert_eq!(
        p.effect_for("media_generate_image", &serde_json::json!({}))
            .group,
        EffectGroup::Spend
    );
    assert_eq!(
        p.effect_for("media_generate_video", &serde_json::json!({}))
            .group,
        EffectGroup::Spend
    );
}

/// What string a per-call **tool** gate actually puts in front of an
/// operator (issue #701).
///
/// The issue could not answer this from the frontend, and declined to
/// invent labels without it — rightly: a card whose job is informed consent
/// is worse off with a label naming the wrong action than with a vague one.
/// The answer is that a tool gate parks under the tool's own raw name.
/// [`ApprovalPolicy::require_approval`] is the only construction site for a
/// `RequireApproval` decision, it builds its request through
/// [`ApprovalPolicy::effect_for`], and that sets `kind` to `tool_name`
/// verbatim; `CompanyRuntime::pending_approvals` — the only projection
/// point for an `ApprovalSummary` — copies it through unchanged.
///
/// Two of the seven invite the opposite guess, so both are pinned here
/// rather than argued in prose:
///
/// * `publish_artifact` does **not** park as `external.publish`. That kind
///   exists only as a native workflow-gate class and a `DEFAULT_ALWAYS_APPROVE`
///   entry; `harness::publish` builds no effect and touches no gate.
/// * `run_workflow` does **not** park as `workflow.approve`. That kind is a
///   workflow *resuming* mid-run (issue #395, `WORKFLOW_APPROVE_KIND`),
///   which is a different event from an agent asking to start one.
///
/// So all seven need entries in the console's tool-label table, and this
/// test is what stops that answer decaying back into a guess.
#[test]
fn parked_kind_is_the_tool_name() {
    let p = policy("supervised", &[], None);
    for tool in [
        "curl",
        "git_operations",
        "http_request",
        "mcp_call_tool",
        "publish_artifact",
        "read_workspace_state",
        "run_workflow",
    ] {
        assert_eq!(
            p.effect_for(tool, &serde_json::json!({})).kind,
            tool,
            "`{tool}` parks under a kind the console's tool-label table does \
             not key on; the label added for it in `language.ts` is now \
             unreachable"
        );
    }
}

/// Per-tenant Composio (issue #110): the read tools are read-only (allowed
/// even under supervised/readonly), while `composio_authorize` /
/// `composio_execute` are external — parked under supervised, denied under
/// readonly.
#[tokio::test]
async fn composio_reads_allowed_but_authorize_execute_park_or_deny() {
    in_cycle(async {
        let supervised = policy("supervised", &[], None);
        for tool in [
            "composio_list_toolkits",
            "composio_list_connections",
            "composio_list_tools",
        ] {
            assert_eq!(
                supervised
                    .check(&request(tool, serde_json::json!({})))
                    .await,
                ToolPolicyDecision::Allow,
                "{tool} is read-only and must be allowed"
            );
        }
        for tool in ["composio_authorize", "composio_execute"] {
            assert!(
                matches!(
                    supervised
                        .check(&request(tool, serde_json::json!({})))
                        .await,
                    ToolPolicyDecision::RequireApproval { .. }
                ),
                "{tool} must park under supervised"
            );
        }

        let readonly = policy("readonly", &[], None);
        // A read-only desk may still browse the Composio surface.
        assert_eq!(
            readonly
                .check(&request("composio_list_connections", serde_json::json!({})))
                .await,
            ToolPolicyDecision::Allow
        );
        for tool in ["composio_authorize", "composio_execute"] {
            assert!(
                matches!(
                    readonly.check(&request(tool, serde_json::json!({}))).await,
                    ToolPolicyDecision::Deny { .. }
                ),
                "{tool} must be denied under readonly"
            );
        }
    })
    .await;
}

/// Composio effect groups (issue #110): authorize is an Identity effect,
/// execute is a Send effect — pinned before the generic `contains`
/// heuristics could misclassify the slug.
#[test]
fn composio_classifies_authorize_identity_and_execute_send() {
    let p = policy("supervised", &[], None);
    assert_eq!(
        p.effect_for("composio_authorize", &serde_json::json!({}))
            .group,
        EffectGroup::Identity
    );
    assert_eq!(
        p.effect_for("composio_execute", &serde_json::json!({}))
            .group,
        EffectGroup::Send
    );
}

/// Metered web search (issue #238): allowed under `supervised`, DENIED
/// under `readonly`, allowed under `full`.
///
/// This is the classification decision the issue got wrong in both
/// directions, so it is pinned here rather than left to the heuristic:
///
/// * It must **not park** under `supervised`. openhuman resolves a
///   `RequireApproval` inline and never re-dispatches the call (module
///   docs), so parking a search is not "the operator approves it later" —
///   it is "the search never happens", leaving the agent in exactly the
///   no-discovery state that makes it invent citations. Consent is the
///   explicit `search` grant; the boundary is the daily cap.
/// * It must **still be denied** under `readonly`. The issue proposed a
///   flat carve-out, which would have made a paid outbound call
///   unstoppable in the one tier whose entire promise is that nothing is
///   spent. `web_fetch`, its sibling in the same research loop, is denied
///   there; a *priced* discovery call has no business being more permissive
///   than the free retrieval call it feeds.
#[tokio::test]
async fn web_search_never_parks_under_supervised_but_is_denied_read_only() {
    let supervised = policy("supervised", &[], None);
    assert_eq!(
        supervised
            .check(&request(
                "web_search",
                serde_json::json!({ "query": "acme pricing" })
            ))
            .await,
        ToolPolicyDecision::Allow,
        "a search must not park: a parked search is a search that never runs"
    );
    assert_eq!(
        supervised.requests.queued(),
        0,
        "an allowed search must not queue an approval request"
    );

    assert_eq!(
        supervised
            .check(&request(
                "web_fetch",
                serde_json::json!({ "url": "https://a.test/" })
            ))
            .await,
        ToolPolicyDecision::Allow,
        "free retrieval must not park under supervised"
    );

    let readonly = policy("readonly", &[], None);
    assert!(
        matches!(
            readonly
                .check(&request(
                    "web_search",
                    serde_json::json!({ "query": "acme" })
                ))
                .await,
            ToolPolicyDecision::Deny { .. }
        ),
        "a read-only desk spends nothing, and a search spends"
    );

    let full = policy("full", &[], None);
    assert_eq!(
        full.check(&request(
            "web_search",
            serde_json::json!({ "query": "acme" })
        ))
        .await,
        ToolPolicyDecision::Allow
    );
}

/// The company workspace (issues #237, #551, #671): the two read tools
/// reach only this company's own note tree and must be allowed in every
/// mode, while the four mutations — `workspace_write` (overwrites shared
/// guidance), `workspace_create` (adds to the tree everyone reads),
/// `workspace_delete` and `workspace_rename` (remove and move what the
/// agent put in its own folder) — must park under supervised / be denied
/// under readonly.
///
/// This is the ACTUAL gate on a workspace write. Issue #237 proposed that
/// declaring `PermissionLevel::Write` would keep the `ApprovalPolicy` as
/// the per-call gate; it would not — openhuman's `ToolPolicy` surface hands
/// this bridge only the tool name and args, never the tool's permission
/// level, so classification is by name alone. Pinning every one of the six
/// names here is what stops a later rename (say `get_workspace_note`, which the
/// read-only prefix list would silently wave through) from moving a tool
/// across the gate unnoticed.
#[tokio::test]
async fn workspace_reads_are_allowed_but_writes_park_or_deny() {
    in_cycle(async {
        let supervised = policy("supervised", &[], None);
        for tool in ["workspace_list", "workspace_read"] {
            assert_eq!(
                supervised
                    .check(&request(tool, serde_json::json!({})))
                    .await,
                ToolPolicyDecision::Allow,
                "{tool} only reads this company's own workspace and must be allowed"
            );
        }
        for tool in [
            "workspace_write",
            "workspace_create",
            "workspace_delete",
            "workspace_rename",
        ] {
            assert!(
                matches!(
                    supervised
                        .check(&request(tool, serde_json::json!({})))
                        .await,
                    ToolPolicyDecision::RequireApproval { .. }
                ),
                "{tool} must park under supervised"
            );
        }

        let readonly = policy("readonly", &[], None);
        for tool in ["workspace_list", "workspace_read"] {
            assert_eq!(
                readonly.check(&request(tool, serde_json::json!({}))).await,
                ToolPolicyDecision::Allow,
                "{tool} must stay available to a read-only desk"
            );
        }
        for tool in [
            "workspace_write",
            "workspace_create",
            "workspace_delete",
            "workspace_rename",
        ] {
            assert!(
                matches!(
                    readonly.check(&request(tool, serde_json::json!({}))).await,
                    ToolPolicyDecision::Deny { .. }
                ),
                "{tool} must be denied under readonly"
            );
        }

        // Under `full` these still run. There IS a per-call gate now (issue
        // #338), but writing the company's own note tree is not one of the acts
        // it stops: it is internal, and the gate is scoped to what leaves the
        // company or cannot be bounded. These tools keep their own safeguards:
        // writes and deletes require an `expected_updated_at` compare-and-swap
        // token, creates refuse paths that already resolve, deletes refuse
        // folders that still hold anything, and renames refuse occupied
        // destinations.
        //
        // This is the assertion that caught the first version of that gate,
        // which stopped both: publishing runs through them, and thirteen
        // `publish_turn_test` cases failed with "the model was never handed a
        // publish receipt".
        let full = policy("full", &[], None);
        for tool in [
            "workspace_write",
            "workspace_create",
            "workspace_delete",
            "workspace_rename",
        ] {
            assert_eq!(
                full.check(&request(tool, serde_json::json!({}))).await,
                ToolPolicyDecision::Allow,
                "{tool} under full mode"
            );
        }
    })
    .await;
}

/// The policy asks whose node a workspace mutation targets, not merely
/// which workspace tool the agent selected. Only a node both created and
/// last written by that agent runs under `auto`; an operator, a teammate,
/// a missing lookup, or an unfamiliar create path keeps the gate.
#[tokio::test]
async fn auto_allows_only_mutations_of_the_callers_own_workspace_work() {
    in_cycle(async {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn WorkspaceStore> = Arc::new(FsOps::new(dir.path()));
        let company = CompanyId::new("acme");
        let node = |id: &str, name: &str, origin: WorkspaceOrigin| WorkspaceNode {
            id: id.to_string(),
            name: name.to_string(),
            kind: NodeKind::File,
            parent_id: None,
            updated_at_millis: 1,
            created_by: origin.clone(),
            updated_by: origin,
            mime: None,
            size: None,
            sha256: None,
            adopted: false,
        };
        let own = WorkspaceOrigin::Agent {
            id: "ceo".to_string(),
        };
        store
            .create(&company, &node("own", "own.md", own), Some("draft"))
            .await
            .unwrap();
        store
            .create(
                &company,
                &node("operator", "operator.md", WorkspaceOrigin::Operator),
                Some("guidance"),
            )
            .await
            .unwrap();
        store
            .create(
                &company,
                &node(
                    "teammate",
                    "teammate.md",
                    WorkspaceOrigin::Agent {
                        id: "cmo".to_string(),
                    },
                ),
                Some("brief"),
            )
            .await
            .unwrap();

        let policy = policy("auto", &[], None)
            .with_agent("ceo")
            .with_workspace(store, company);
        for (tool, args) in [
            ("workspace_write", serde_json::json!({ "id": "own" })),
            ("workspace_delete", serde_json::json!({ "id": "own" })),
            ("workspace_rename", serde_json::json!({ "id": "own" })),
            (
                "workspace_create",
                serde_json::json!({ "path": "agents/ceo/draft.md" }),
            ),
        ] {
            assert_eq!(
                policy.check(&request(tool, args)).await,
                ToolPolicyDecision::Allow,
                "{tool}"
            );
        }
        for args in [
            serde_json::json!({ "id": "operator" }),
            serde_json::json!({ "id": "teammate" }),
            serde_json::json!({ "id": "missing" }),
        ] {
            assert!(matches!(
                policy.check(&request("workspace_write", args)).await,
                ToolPolicyDecision::RequireApproval { .. }
            ));
        }
        assert!(matches!(
            policy
                .check(&request(
                    "workspace_create",
                    serde_json::json!({ "path": "standards/new.md" })
                ))
                .await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
    })
    .await;
}

/// Authorship narrows the auto tier only. Even an agent's own note is a
/// state change, so supervised still presents it and readonly still denies
/// it; `always_approve` remains the operator's explicit override.
#[tokio::test]
async fn ownership_never_relaxes_supervised_or_readonly_workspace_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn WorkspaceStore> = Arc::new(FsOps::new(dir.path()));
    let company = CompanyId::new("acme");
    let own = WorkspaceOrigin::Agent {
        id: "ceo".to_string(),
    };
    store
        .create(
            &company,
            &WorkspaceNode {
                id: "own".to_string(),
                name: "own.md".to_string(),
                kind: NodeKind::File,
                parent_id: None,
                updated_at_millis: 1,
                created_by: own.clone(),
                updated_by: own,
                mime: None,
                size: None,
                sha256: None,
                adopted: false,
            },
            Some("draft"),
        )
        .await
        .unwrap();
    for mode in ["supervised", "readonly"] {
        let policy = policy(mode, &[], None)
            .with_agent("ceo")
            .with_workspace(store.clone(), company.clone());
        let decision = policy
            .check(&request(
                "workspace_write",
                serde_json::json!({ "id": "own" }),
            ))
            .await;
        assert!(
            matches!(
                decision,
                ToolPolicyDecision::RequireApproval { .. } | ToolPolicyDecision::Deny { .. }
            ),
            "{mode}"
        );
    }
}

/// A folder rename re-renders the path of every node inside it, so the
/// auto-tier exception for `workspace_rename` is not target-only: an
/// agent-created folder that has since gained an operator- or teammate-
/// authored node must restore the approval gate, while a folder holding
/// only the agent's own work still runs unattended.
#[tokio::test]
async fn auto_rename_of_a_folder_checks_every_descendants_authorship() {
    in_cycle(async {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn WorkspaceStore> = Arc::new(FsOps::new(dir.path()));
        let company = CompanyId::new("acme");
        let own = WorkspaceOrigin::Agent {
            id: "ceo".to_string(),
        };
        let node = |id: &str,
                    name: &str,
                    kind: NodeKind,
                    parent: Option<&str>,
                    origin: WorkspaceOrigin| {
            WorkspaceNode {
                id: id.to_string(),
                name: name.to_string(),
                kind,
                parent_id: parent.map(str::to_string),
                updated_at_millis: 1,
                created_by: origin.clone(),
                updated_by: origin,
                mime: None,
                size: None,
                sha256: None,
                adopted: false,
            }
        };
        store
            .create(
                &company,
                &node("own", "own", NodeKind::Folder, None, own.clone()),
                None,
            )
            .await
            .unwrap();
        store
            .create(
                &company,
                &node("mixed", "mixed", NodeKind::Folder, None, own.clone()),
                None,
            )
            .await
            .unwrap();
        store
            .create(
                &company,
                &node(
                    "own-note",
                    "own-note.md",
                    NodeKind::File,
                    Some("own"),
                    own.clone(),
                ),
                Some("mine"),
            )
            .await
            .unwrap();
        store
            .create(
                &company,
                &node(
                    "operator-note",
                    "operator-note.md",
                    NodeKind::File,
                    Some("mixed"),
                    WorkspaceOrigin::Operator,
                ),
                Some("theirs"),
            )
            .await
            .unwrap();

        let policy = policy("auto", &[], None)
            .with_agent("ceo")
            .with_workspace(store, company);
        assert_eq!(
            policy
                .check(&request(
                    "workspace_rename",
                    serde_json::json!({ "id": "own" })
                ))
                .await,
            ToolPolicyDecision::Allow
        );
        assert!(matches!(
            policy
                .check(&request(
                    "workspace_rename",
                    serde_json::json!({ "id": "mixed" })
                ))
                .await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
    })
    .await;
}

/// A rename that *moves* a node has the same landing-zone rule
/// `workspace_create` applies to minting one: an agent-owned note moved
/// into an operator-authored folder inside the agent's home must restore
/// the approval gate, or the operator-created folder becomes an unreviewed
/// collection point. The home root keeps the exception — it is the agent's
/// own space whatever its stored origin.
#[tokio::test]
async fn auto_rename_into_a_foreign_folder_inside_the_home_parks() {
    in_cycle(async {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn WorkspaceStore> = Arc::new(FsOps::new(dir.path()));
        let company = CompanyId::new("acme");
        let own = WorkspaceOrigin::Agent {
            id: "ceo".to_string(),
        };
        let node =
            |id: &str, name: &str, parent: Option<&str>, origin: WorkspaceOrigin| WorkspaceNode {
                id: id.to_string(),
                name: name.to_string(),
                kind: if id.starts_with("n-") {
                    NodeKind::File
                } else {
                    NodeKind::Folder
                },
                parent_id: parent.map(str::to_string),
                updated_at_millis: 1,
                created_by: origin.clone(),
                updated_by: origin,
                mime: None,
                size: None,
                sha256: None,
                adopted: false,
            };
        store
            .create(&company, &node("agents", "agents", None, own.clone()), None)
            .await
            .unwrap();
        store
            .create(
                &company,
                &node("home", "ceo", Some("agents"), own.clone()),
                None,
            )
            .await
            .unwrap();
        store
            .create(
                &company,
                &node("inbox", "inbox", Some("home"), WorkspaceOrigin::Operator),
                None,
            )
            .await
            .unwrap();
        store
            .create(
                &company,
                &node("n-own", "own.md", Some("home"), own.clone()),
                Some("mine"),
            )
            .await
            .unwrap();

        let policy = policy("auto", &[], None)
            .with_agent("ceo")
            .with_workspace(store, company);

        // Into the operator-authored folder: the approval gate comes back.
        assert!(matches!(
            policy
                .check(&request(
                    "workspace_rename",
                    serde_json::json!({
                        "path": "agents/ceo/own.md",
                        "new_parent": "agents/ceo/inbox"
                    })
                ))
                .await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
        // Into the home root: the agent's own space, no approval needed.
        assert_eq!(
            policy
                .check(&request(
                    "workspace_rename",
                    serde_json::json!({
                        "path": "agents/ceo/own.md",
                        "new_parent": "agents/ceo"
                    })
                ))
                .await,
            ToolPolicyDecision::Allow
        );
    })
    .await;
}

/// The operator's escape hatch: `always_approve` wins over every tier, so a
/// company that *does* want to eyeball each paid search can have that —
/// and the parked request is projected as a **spend**, not the catch-all
/// "other", so the Approvals page says what is being approved.
#[tokio::test]
async fn an_operator_can_still_force_approval_on_each_search() {
    in_cycle(async {
        let policy = policy("supervised", &["web_search"], None);
        assert!(
            matches!(
                policy
                    .check(&request(
                        "web_search",
                        serde_json::json!({ "query": "acme" })
                    ))
                    .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "`always_approve` must override the metered-read carve-out"
        );
        assert_eq!(
            policy
                .effect_for("web_search", &serde_json::json!({}))
                .group,
            EffectGroup::Spend,
            "a paid call must not park as an unlabelled `Other`"
        );
    })
    .await;
}

#[test]
fn effect_projection_infers_group_and_amount() {
    let p = policy("supervised", &[], None);
    let effect = p.effect_for("pay_invoice", &serde_json::json!({ "amount_usd": 12.5 }));
    assert_eq!(effect.kind, "pay_invoice");
    assert_eq!(effect.group, EffectGroup::Spend);
    assert_eq!(effect.amount_usd, Some(12.5));
}
