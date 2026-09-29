use super::*;

// Issue #470: the `composio_execute` fixtures are built here, from the same
// key the classifier reads, so a call in a test reaches the same catalogue
// lookup a call in production does.
use super::policy_test_helpers_tests::*;
use crate::policy::test_support::{composio_args, composio_unclassified_args};

/// The sibling defects the same sweep turned up. Four pure reads of the
/// agent's own workspace parked under the default mode, because the
/// read-only rule matched a *prefix* and none of these names begins with a
/// read-only word. Nobody had reported them; they were found by asking the
/// same question of every registered tool.
///
/// `read_workspace_state` was in this list until issue #459 — see
/// [`reading_workspace_state_parks_supervised_and_denies_readonly`].
#[tokio::test]
async fn a_workspace_read_runs_without_asking_whatever_its_name_begins_with() {
    let p = policy("supervised", &[], None);
    for tool in [
        "file_read",
        "glob",
        "grep",
        "image_info",
        "list",
        "memory_recall",
    ] {
        assert_eq!(
            p.check(&request(tool, serde_json::json!({}))).await,
            ToolPolicyDecision::Allow,
            "`{tool}` reads the agent's own workspace"
        );
    }
}

/// Issue #459: the sibling that turned out not to be a read at all. It
/// shells out to `git status` in the agent's own workspace, and the
/// vendored `run_git` lets that directory's `.git/config` — which
/// `file_write` can author — decide what git executes. So it goes through
/// the gate the way `shell` does, and this is the assertion an operator
/// actually feels.
#[tokio::test]
async fn reading_workspace_state_parks_supervised_and_denies_readonly() {
    in_cycle(async {
        assert!(
            matches!(
                policy("supervised", &[], None)
                    .check(&request("read_workspace_state", serde_json::json!({})))
                    .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "running git under agent-authored config must reach an operator"
        );
        assert!(
            matches!(
                policy("readonly", &[], None)
                    .check(&request("read_workspace_state", serde_json::json!({})))
                    .await,
                ToolPolicyDecision::Deny { .. }
            ),
            "`readonly` promises nothing runs; a git config key can name a command"
        );
    })
    .await;
}

/// …and the `readonly` denial says **why**, because this is the one an
/// operator ends up confused by.
///
/// `read_workspace_state` used to run on a `readonly` desk, so a company
/// that sat there gets a refusal on what was a normal first move — and a
/// tier that promises reads still work refusing something called `read_*`
/// reads as a bug in the tier unless the message names git. The
/// `supervised` park explains itself by producing a card to approve; this
/// one has to carry its reason.
#[tokio::test]
async fn the_readonly_denial_of_a_read_shaped_tool_says_why() {
    let ToolPolicyDecision::Deny { reason } = policy("readonly", &[], None)
        .check(&request("read_workspace_state", serde_json::json!({})))
        .await
    else {
        panic!("`readonly` denies it");
    };
    assert!(
        reason.contains("git"),
        "an operator must be able to tell this from a mis-classification: {reason}"
    );
    assert!(
        reason.contains("workspace"),
        "and where the config it obeys comes from: {reason}"
    );

    // A tool whose name already argues for the verdict carries no such
    // clause — every denial ending in an explanation is an explanation
    // nobody reads.
    let ToolPolicyDecision::Deny { reason } = policy("readonly", &[], None)
        .check(&request("shell", serde_json::json!({})))
        .await
    else {
        panic!("`readonly` denies shell");
    };
    assert!(!reason.contains("git"), "{reason}");
}

/// A standing grant admits any arguments, which was a fair summary of a
/// tool's consequence while consequence was a property of the tool name.
/// It is not one for `composio_execute`, so the grant is re-checked against
/// the live call: a scope granted on a repository read must not admit an
/// outgoing email on the same handle.
#[tokio::test]
async fn a_standing_grant_on_a_composio_read_does_not_admit_a_send() {
    in_cycle(async {
        let queue = ApprovalRequestQueue::default();
        let grants = queue.grants();
        let p = policy("supervised", &[], None)
            .with_requests(queue)
            .with_agent("ops");
        grants.grant_standing(standing("ops", "composio_execute", far_future()));

        assert_eq!(
            p.check(&request(
                "composio_execute",
                serde_json::json!({ "tool": "GITHUB_LIST_PULL_REQUESTS" })
            ))
            .await,
            ToolPolicyDecision::Allow,
            "the read the operator granted keeps running"
        );
        assert!(
            matches!(
                p.check(&request(
                    "composio_execute",
                    serde_json::json!({ "tool": "GMAIL_SEND_EMAIL" })
                ))
                .await,
                ToolPolicyDecision::RequireApproval { .. }
            ),
            "a send on the same tool name parks despite the grant"
        );
    })
    .await;
}

/// External reads are decided before standing grants are consulted.
#[tokio::test]
async fn a_fetch_grant_does_not_narrow_free_external_reads() {
    in_cycle(async {
        for tier in ["supervised", "auto"] {
            let queue = ApprovalRequestQueue::default();
            let grants = queue.grants();
            let p = policy(tier, &[], None)
                .with_requests(queue)
                .with_agent("ops");
            grants.grant_standing(scoped_standing(
                "ops",
                crate::policy::consequence::WEB_FETCH,
                "https://docs.rs",
                far_future(),
            ));

            assert!(
                matches!(
                    p.check(&request(
                        "web_fetch",
                        serde_json::json!({ "url": "https://docs.rs/serde" })
                    ))
                    .await,
                    ToolPolicyDecision::Allow
                ),
                "a second fetch of the granted host must run unattended under `{tier}`"
            );

            assert_eq!(
                p.check(&request(
                    "web_fetch",
                    serde_json::json!({ "url": "https://crates.io/crates/serde" })
                ))
                .await,
                ToolPolicyDecision::Allow,
                "external reads do not depend on a host grant — `{tier}`"
            );

            assert_eq!(
                p.check(&request(
                    "web_fetch",
                    serde_json::json!({ "url": "https://evil.docs.rs/x" })
                ))
                .await,
                ToolPolicyDecision::Allow,
                "a subdomain is still an external read — `{tier}`"
            );

            assert!(
                matches!(
                    p.check(&request(
                        "web_fetch",
                        serde_json::json!({ "url": "not-a-url" })
                    ))
                    .await,
                    ToolPolicyDecision::RequireApproval { .. }
                ),
                "a URL with no readable host stays gated rather than free — `{tier}`"
            );
        }
    })
    .await;
}

/// Existing batch grants do not turn external reads back into parked calls.
#[tokio::test]
async fn batch_grants_leave_external_reads_free() {
    for tier in ["supervised", "auto"] {
        let queue = ApprovalRequestQueue::default();
        let grants = queue.grants();
        let p = policy(tier, &[], None)
            .with_requests(queue)
            .with_agent("seo");

        grants.grant_standing(scoped_standing(
            "seo",
            crate::policy::consequence::WEB_FETCH,
            "https://espn.com",
            far_future(),
        ));
        grants.grant_standing(crate::runtime::grants::StandingGrant {
            id: crate::runtime::grants::GrantId::new("g2"),
            ..scoped_standing(
                "seo",
                crate::policy::consequence::WEB_FETCH,
                "https://bbc.com",
                far_future(),
            )
        });

        for url in [
            "https://espn.com/nba/scores",
            "https://bbc.com/sport/football",
        ] {
            assert!(
                matches!(
                    p.check(&request("web_fetch", serde_json::json!({ "url": url })))
                        .await,
                    ToolPolicyDecision::Allow
                ),
                "an approved item's own host-scoped grant must admit it — `{tier}`: {url}"
            );
        }

        assert_eq!(
            p.check(&request(
                "web_fetch",
                serde_json::json!({ "url": "https://theguardian.com/uk" })
            ))
            .await,
            ToolPolicyDecision::Allow,
            "an external read does not need a batch grant — `{tier}`"
        );

        assert_eq!(
            p.check(&request(
                "web_fetch",
                serde_json::json!({ "url": "https://crates.io/crates/serde" })
            ))
            .await,
            ToolPolicyDecision::Allow,
            "an unrelated external read remains free — `{tier}`"
        );
    }
}

/// External reads run without a standing grant in both active tiers.
#[tokio::test]
async fn an_ungranted_fetch_runs_under_supervised_and_auto() {
    for tier in ["supervised", "auto"] {
        let p = policy(tier, &[], None);
        assert_eq!(
            p.check(&request(
                "web_fetch",
                serde_json::json!({ "url": "https://docs.rs/serde" })
            ))
            .await,
            ToolPolicyDecision::Allow,
            "an outward read runs without a grant under `{tier}`"
        );
    }
}

/// Issue #457's scope check, pinned **directly** rather than through
/// `check()`.
///
/// It used to run through the real admission path, asserting that a grant
/// scoped to `github` admitted a GitHub read and re-parked a Gmail one.
/// Issue #559 made that unobservable from `check()`, and the honest thing
/// is to say so rather than relax the assertion until it passes:
///
/// * under `supervised` a catalogue read no longer parks at all, so it is
///   allowed by the tier long before the scope is consulted;
/// * under `readonly` the brake at step 1 denies every external effect
///   *above* the grant checks, so the scope is not consulted there either;
/// * under `full` everything is allowed.
///
/// So `standing_grant_allows` is still correct and still worth pinning —
/// this test does that — but no tier currently routes a Composio read to
/// it. See the note in the PR for #559; the issue's claim that
/// `Standing::Grantable` "still governs `readonly`" does not hold against
/// the ordering in `check()`.
#[tokio::test]
async fn a_grant_scoped_to_one_provider_does_not_admit_another_providers_read() {
    in_cycle(async {
        let queue = ApprovalRequestQueue::default();
        let grants = queue.grants();
        let p = policy("supervised", &[], None)
            .with_requests(queue)
            .with_agent("ops");
        grants.grant_standing(scoped_standing(
            "ops",
            "composio_execute",
            "github",
            far_future(),
        ));

        // A *different* GitHub read: the operator consented to the provider, so
        // this is inside the sentence. Scoping by action slug instead would
        // have refused here and made the grant worthless.
        assert!(
            p.standing_grant_allows(
                "composio_execute",
                &composio_args("GITHUB_LIST_PULL_REQUESTS")
            ),
            "the operator consented to a provider, not to one action slug"
        );

        // A mailbox read. Also a catalogue read, also grantable, also `ops`,
        // also `composio_execute` — every check upstream of the scope says yes,
        // and the scope is the one thing that says no.
        assert!(
            !p.standing_grant_allows("composio_execute", &composio_args("GMAIL_FETCH_EMAILS")),
            "'read from GitHub' is not consent to read the company's mail"
        );

        // An action the catalogue cannot place carries no scope, so a scoped
        // grant refuses it — unknown is a send, here too.
        assert!(
            !p.standing_grant_allows("composio_execute", &composio_unclassified_args()),
            "an unplaceable action has no scope for a scoped grant to admit"
        );

        // Through the real gate, the unknown action still parks: it is a send,
        // so the tier does not wave it through and the scoped grant will not
        // admit it either. This half of the original test survives #559
        // unchanged, because only the *read* branch moved.
        assert!(matches!(
            p.check(&request("composio_execute", composio_unclassified_args()))
                .await,
            ToolPolicyDecision::RequireApproval { .. }
        ));

        // Deliberately NOT asserting `check(read) == Allow` here. It would pass
        // whether or not #559 landed — this policy holds a standing grant that
        // admits a GitHub read at step 2b, so the tier never gets a say, and
        // the assertion would prove nothing while looking like it proved the
        // change. `a_composio_read_runs_under_supervision_without_parking` is
        // the test for that, and it uses a policy with no grant at all.

        assert_eq!(
            grants.standing_count(),
            1,
            "none of those refusals spent the permission"
        );
    })
    .await;
}

/// Companion to `a_grant_scoped_to_one_provider_does_not_admit_another_providers_read`,
/// for `web_fetch`'s own host scope. Pinned directly against
/// `standing_grant_allows` for the same reason that one is: under
/// `supervised`/`auto`, `Reach::ExternalRead` no longer parks, so `check()`
/// allows an external read long before any grant is consulted and a
/// `check()`-level assertion would prove nothing about scope enforcement
/// (`batch_grants_leave_external_reads_free` documents exactly that). The
/// scope machinery — `admits_scope`'s exact host match — still exists and
/// still refuses a grant with the wrong scope; this is the test that says
/// so.
#[test]
fn a_web_fetch_grant_scoped_to_one_host_does_not_admit_another() {
    let queue = ApprovalRequestQueue::default();
    let grants = queue.grants();
    let p = policy("supervised", &[], None)
        .with_requests(queue)
        .with_agent("seo");
    grants.grant_standing(scoped_standing(
        "seo",
        crate::policy::consequence::WEB_FETCH,
        "https://espn.com",
        far_future(),
    ));

    assert!(
        p.standing_grant_allows(
            "web_fetch",
            &serde_json::json!({ "url": "https://espn.com/nba/scores" })
        ),
        "the granted host admits its own fetch"
    );

    assert!(
        !p.standing_grant_allows(
            "web_fetch",
            &serde_json::json!({ "url": "https://bbc.com/sport/football" })
        ),
        "a grant scoped to one host does not admit a different one"
    );

    assert!(
        !p.standing_grant_allows("web_fetch", &serde_json::json!({ "url": "not-a-url" })),
        "an unresolvable URL has no scope for a scoped grant to admit"
    );

    assert_eq!(
        grants.standing_count(),
        1,
        "none of those checks spent the permission"
    );
}

/// **Replay compatibility (issue #457).** A grant journaled before the scope
/// field existed comes back unscoped, and an unscoped grant admits the tool
/// exactly as it did before — otherwise this change would silently void
/// every permission an operator had already granted.
#[tokio::test]
async fn a_grant_from_before_scopes_existed_still_admits_its_tool() {
    in_cycle(async {
        let queue = ApprovalRequestQueue::default();
        let grants = queue.grants();
        let p = policy("supervised", &[], None)
            .with_requests(queue)
            .with_agent("ops");

        // Deserialized from the pre-#457 wire shape rather than constructed, so
        // this fails if the field ever stops defaulting.
        let replayed: crate::runtime::grants::StandingGrant =
            serde_json::from_value(serde_json::json!({
                "id": "g-old",
                "agent": "ops",
                "tool": "composio_execute",
                "granted_by": { "kind": "user", "id": "user-1" },
                "approval_id": "appr-old",
                "at_millis": 1_000,
                "expires_at_millis": far_future(),
            }))
            .expect("an old journal line still replays");
        assert_eq!(replayed.scope, None);
        grants.grant_standing(replayed);

        for slug in ["GITHUB_LIST_PULL_REQUESTS", "GMAIL_FETCH_EMAILS"] {
            assert_eq!(
                p.check(&request(
                    "composio_execute",
                    serde_json::json!({ "tool": slug })
                ))
                .await,
                ToolPolicyDecision::Allow,
                "an unscoped grant behaves exactly as it did: {slug}"
            );
        }
        // …and the boundary that was always there is untouched: a send still
        // parks, because the live re-classification runs first.
        assert!(matches!(
            p.check(&request(
                "composio_execute",
                serde_json::json!({ "tool": "GMAIL_SEND_EMAIL" })
            ))
            .await,
            ToolPolicyDecision::RequireApproval { .. }
        ));
    })
    .await;
}

/// Issue #374 added the `deploy` arm. It still applies — to tools with no
/// declaration, which is now the only place the name heuristics run.
#[test]
fn an_undeclared_deploy_still_classifies_as_publish() {
    let args = serde_json::json!({});
    assert_eq!(classify_group("deploy_site", &args), EffectGroup::Publish);
    assert_eq!(
        classify_group("website_deploy", &args),
        EffectGroup::Publish
    );
    assert_eq!(classify_group("publish_post", &args), EffectGroup::Publish);
    // …but it no longer decides grantability, so a deploy tool nobody has
    // declared is refused a standing scope by the undeclared rule as well
    // as by its group.
    assert!(!grantable("deploy_site", &args));
}

/// The four workspace mutations keep their `Other` label on the card —
/// there is no consequence word to name — while being refused a standing
/// scope.
/// That separation is the point of issue #444: the label and the permission
/// are different questions.
#[test]
fn workspace_mutations_are_labelled_other_and_are_still_not_grantable() {
    let args = serde_json::json!({});
    for tool in [
        "workspace_write",
        "workspace_create",
        "workspace_delete",
        "workspace_rename",
    ] {
        assert_eq!(classify_group(tool, &args), EffectGroup::Other, "{tool}");
        assert!(classify_group(tool, &args).is_unclassified(), "{tool}");
        assert!(!grantable(tool, &args), "{tool}");
        assert!(is_external_effect(tool, &args), "{tool}");
    }
}
