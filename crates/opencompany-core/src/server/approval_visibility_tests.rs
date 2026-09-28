use super::*;
use crate::ports::types::{ApprovalId, CompanyId};
use crate::ports::{SessionKind, UserRole};
use crate::server::graphql::auth::UserPrincipal;
use crate::server::platform_auth::PlatformClaims;

fn principal(role: UserRole) -> GqlAuth {
    GqlAuth::User(UserPrincipal {
        company: CompanyId::new("acme"),
        user_id: "u-1".to_string(),
        email: "who@example.test".to_string(),
        role,
        must_change_password: false,
        session_token_hash: "hash".to_string(),
        credential: SessionKind::Browser,
    })
}

/// A summary with both contents fields populated — the only interesting
/// input, since redaction is a no-op on an approval that carries neither.
fn summary() -> ApprovalSummary {
    ApprovalSummary {
        id: ApprovalId::new("appr-1"),
        kind: "email.send".to_string(),
        group: crate::ports::types::EffectGroup::Send,
        amount_usd: Some(2400.0),
        at_millis: 1_000,
        expires_at_millis: Some(87_400_000),
        task: None,
        agent: Some("ops".to_string()),
        payload: Some(serde_json::json!({ "to": "board@example.test" })),
        thread: None,
        workflow_run_id: None,
        workflow_id: None,
        broadly_grantable: false,
        broadly_deniable: false,
        contents_hidden: false,
        batch: Some("turn-1".to_string()),
        group_key: None,
        blocker_step_kind: None,
        episode: None,
    }
}

#[test]
fn an_admin_reads_the_contents_unchanged() {
    let out = for_principal(&principal(UserRole::Admin), vec![summary()]);
    assert!(out[0].payload.is_some(), "an admin decides the sign-off");
    assert_eq!(out[0].amount_usd, Some(2400.0));
    assert!(!out[0].contents_hidden);
}

/// The case the issue exists for. Membership still gets the row — that is
/// #468's stalled-work signal — but not what is inside it.
#[test]
fn a_member_gets_the_approval_without_its_contents() {
    let out = for_principal(&principal(UserRole::Member), vec![summary()]);
    assert!(
        out[0].payload.is_none(),
        "the recipient must not reach a member"
    );
    assert!(out[0].amount_usd.is_none(), "nor the amount");
    assert!(
        out[0].contents_hidden,
        "and the console must be able to say so, rather than render an empty card"
    );
    // Everything that makes stalled work legible survives.
    assert_eq!(out[0].kind, "email.send");
    assert_eq!(out[0].agent.as_deref(), Some("ops"));
    assert_eq!(out[0].at_millis, 1_000);
    // Issue #842: including which requests arrived together. Which turn
    // asked is not *contents* — withholding it would split one batch into
    // unrelated single cards for a member, so the two roles would see the
    // conversation interrupted a different number of times for the same
    // turn. Less detail than an admin gets; the same shape of request.
    assert_eq!(
        out[0].batch.as_deref(),
        Some("turn-1"),
        "role redaction withholds contents, not the grouping"
    );
    // **T11 (issue #971).** The deadline is not contents either, and it is
    // the one field whose absence would actively mislead: a member watching
    // their own stalled work would see a card silently vanish with no
    // warning it was going to, which is the failure shortening the deadline
    // would otherwise introduce. Money and recipients stay withheld — the
    // two assertions above — so this widens nothing.
    assert_eq!(
        out[0].expires_at_millis,
        Some(87_400_000),
        "a member must be told when their stalled work will be given up on"
    );
}

/// Issue #618's stated trap: a platform bearer carries no `UserRole`, and
/// whatever it gets must be a decision rather than a fallthrough. It is
/// **fail-closed** — see the comment on `may_read_approval_contents`.
#[test]
fn a_platform_bearer_is_refused_the_contents_explicitly() {
    let claims = PlatformClaims {
        tenant: "tenant:hosting".to_string(),
        scopes: std::collections::HashSet::new(),
        companies: None,
    };
    let out = for_principal(&GqlAuth::Platform(claims), vec![summary()]);
    assert!(out[0].payload.is_none());
    assert!(out[0].amount_usd.is_none());
    assert!(out[0].contents_hidden);
}

/// Redaction must not invent contents where there were none: a no-argument
/// approval read by an admin still reports `contents_hidden == false`, so
/// "nothing to show" and "not shown to you" stay distinguishable.
#[test]
fn an_absent_payload_is_not_reported_as_hidden() {
    let mut bare = summary();
    bare.payload = None;
    bare.amount_usd = None;
    let out = for_principal(&principal(UserRole::Admin), vec![bare]);
    assert!(!out[0].contents_hidden);
}

/// Issue #1418: the workflow origin's *second half* survives redaction.
///
/// `workflow_run_id` already rides through `hide_contents` untouched — it is
/// structural, not contents. The workflow id must too, or a member holding
/// up a stalled native `workflow.approve` would keep the run id and lose the
/// one thing that turns it into an address: exactly the stalled-work
/// visibility issue #468 exists to protect.
#[test]
fn a_member_keeps_the_workflow_origin_when_contents_are_hidden() {
    let mut gate = summary();
    gate.kind = "workflow.approve".to_string();
    gate.payload =
        Some(serde_json::json!({ "workflow_id": "feature_pipeline", "node_id": "spec" }));
    gate.workflow_id = Some("feature_pipeline".to_string());
    let out = for_principal(&principal(UserRole::Member), vec![gate]);
    assert!(out[0].payload.is_none(), "contents stay withheld");
    assert!(
        out[0].contents_hidden,
        "and the card still says it may not show them"
    );
    assert_eq!(
        out[0].workflow_id.as_deref(),
        Some("feature_pipeline"),
        "the workflow origin is an address, not contents, and survives"
    );
}
