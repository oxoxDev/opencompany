use crate::ports::types::{CompanyId, SecretValue};
use crate::server::ops::ConnectionsRuntime;
use crate::server::ops::mailer::{MailCredentials, RecordingMailSender};
use crate::server::ops::smtp::{SmtpCredentials, SmtpSecurity};
use crate::server::router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;

use super::routes_test_support_1::*;

/// An injected address the manifest does not name renders as its own
/// `platform:` row, so the invite page does not contradict who can log in — and
/// revoking it is refused, pointing at the variable rather than the manifest.
#[tokio::test]
async fn the_env_admin_shows_on_the_invite_page_and_cannot_be_revoked() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, sender) = state_with_admin_email(&home, manifest(), Some("zoe@example.com")).await;
    let admin = login_via_link(&state, &sender, "ada@example.com").await;

    let app = router(state.clone());
    let response = app
        .oneshot(get_with_cookie(
            "/api/v1/companies/acme/users/invites",
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let invites = body_json(response).await;
    let rows = invites.as_array().expect("invite list");
    assert_eq!(rows.len(), 1, "expected one synthetic row: {invites}");
    assert_eq!(rows[0]["id"], "platform:zoe@example.com");
    assert_eq!(rows[0]["invitedBy"], "platform");
    assert_eq!(rows[0]["role"], "admin");

    let app = router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/companies/acme/users/invites/platform:zoe@example.com")
                .header("cookie", &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "revoking would be a lie: the variable re-grants on the next login"
    );
    assert!(
        body_json(response).await["error"]
            .as_str()
            .unwrap()
            .contains("OPENCOMPANY_ADMIN_EMAIL")
    );
}

#[tokio::test]
async fn inviting_someone_mails_them_a_credential_free_invitation() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, sender) = state_with_mail(&home).await;
    let admin = login_via_link(&state, &sender, "ada@example.com").await;
    let before = sender.sent().len();

    let (status, body) = invite_as(&state, &admin, "bob@example.com").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["delivery"], "sent");
    // The record's own fields stay top level: the response is additive.
    assert_eq!(body["email"], "bob@example.com");
    assert!(
        body["notifiedAtMillis"].is_number(),
        "a sent invite must record when: {body}"
    );

    let sent = sender.sent();
    assert_eq!(
        sent.len() - before,
        1,
        "inviting once must send exactly one mail"
    );
    let mail = &sent.last().unwrap().1;
    assert_eq!(mail.to, "bob@example.com");
    assert!(
        mail.subject.contains("Acme"),
        "the subject must name the company: {}",
        mail.subject
    );
    assert!(
        mail.body.contains("/login?company=acme"),
        "the mail must say where to sign in: {}",
        mail.body
    );

    // The property the issue's acceptance criteria turn on: this is a
    // notification, not a credential. The allowlist plus the magic link stays
    // the only way in, so nothing redeemable may appear in the body.
    let invite_id = body["id"].as_str().expect("an invite id");
    assert!(
        !mail.body.contains(invite_id),
        "the invite id must not travel in the mail: {}",
        mail.body
    );
    assert!(
        !mail.body.contains("code="),
        "the mail must carry no login code: {}",
        mail.body
    );
    // The inviter is named from the local part, never by full address — and
    // through `UserRecord::display_label`, so the name in this mail is the one
    // the invitee will meet in the console a minute later.
    assert!(
        mail.body.contains("Ada"),
        "the mail must name who invited them: {}",
        mail.body
    );
    assert!(
        !mail.body.contains("ada@example.com"),
        "the inviter's full address must not be disclosed: {}",
        mail.body
    );

    // And the stamp is durable, not just echoed in the response.
    let app = router(state.clone());
    let response = app
        .oneshot(get_with_cookie(
            "/api/v1/companies/acme/users/invites",
            &admin,
        ))
        .await
        .unwrap();
    let invites = body_json(response).await;
    let row = invites
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["email"] == "bob@example.com")
        .expect("the invite must be listed");
    assert!(
        row["notifiedAtMillis"].is_number(),
        "the mailed stamp must survive the store: {row}"
    );
}

#[tokio::test]
async fn inviting_on_a_host_with_no_mail_says_so_instead_of_reporting_success() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    // No transport at all — the shape the issue calls out as "fails quietly".
    let state = state_with(&home, ConnectionsRuntime::new()).await;
    let admin = login_via_dev_code(&state, "ada@example.com").await;

    let (status, body) = invite_as(&state, &admin, "bob@example.com").await;
    assert_eq!(status, StatusCode::OK, "the grant still succeeds");
    assert_eq!(
        body["delivery"], "no_transport",
        "the operator must be told nothing was mailed: {body}"
    );
    assert!(
        body["notifiedAtMillis"].is_null(),
        "nothing was mailed, so nothing may be stamped: {body}"
    );

    // The invite itself is real — the person can sign in, they just have to be
    // told out of band. Reporting no_transport must not have skipped the grant.
    let app = router(state.clone());
    let response = app
        .oneshot(get_with_cookie(
            "/api/v1/companies/acme/users/invites",
            &admin,
        ))
        .await
        .unwrap();
    let invites = body_json(response).await;
    assert!(
        invites
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["email"] == "bob@example.com"),
        "the invite must exist regardless of mail: {invites}"
    );
}

#[tokio::test]
async fn a_failing_transport_is_reported_and_never_rolls_back_the_invite() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, accepted) = state_refusing_mail_to(&home, "bob@example.com").await;
    let admin = login_via_link(&state, &accepted, "ada@example.com").await;
    let before = accepted.sent().len();

    let (status, body) = invite_as(&state, &admin, "bob@example.com").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a mail failure must not fail the grant"
    );
    assert_eq!(
        body["delivery"], "failed",
        "a refused message must be reported, not swallowed: {body}"
    );
    assert!(
        body["notifiedAtMillis"].is_null(),
        "nothing arrived, so nothing may be stamped as sent: {body}"
    );
    assert_eq!(
        accepted.sent().len(),
        before,
        "the refused message must not appear as delivered"
    );

    // The grant survives the failed send. This is the half that must not
    // regress: rolling the invite back would turn a mail outage into a silent
    // refusal to add people, and re-inviting would then 409 against a record
    // the operator was told did not exist.
    let app = router(state.clone());
    let response = app
        .oneshot(get_with_cookie(
            "/api/v1/companies/acme/users/invites",
            &admin,
        ))
        .await
        .unwrap();
    let invites = body_json(response).await;
    assert!(
        invites
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["email"] == "bob@example.com"),
        "the invite must survive a failed send: {invites}"
    );
}

#[tokio::test]
async fn an_invite_revoked_while_its_mail_is_in_flight_stays_revoked() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let accepted = RecordingMailSender::new();
    let runtime_cell = Arc::new(std::sync::OnceLock::new());
    let connections = ConnectionsRuntime::new()
        .with_mail(Arc::new(RevokingMailSender {
            runtime: runtime_cell.clone(),
            revoke_for: "bob@example.com".to_string(),
            accepted: accepted.clone(),
        }))
        .with_mail_credentials(MailCredentials::Smtp(SmtpCredentials {
            host: "smtp.test".into(),
            port: 587,
            security: SmtpSecurity::Starttls,
            username: "u".into(),
            password: SecretValue("p".into()),
            from_name: "Acme".into(),
            from_email: "noreply@acme.test".into(),
        }));
    let state = state_with(&home, connections).await;
    let _ = runtime_cell.set(
        state
            .registry()
            .get(&CompanyId::new("acme"))
            .expect("the company is registered"),
    );
    let admin = login_via_link(&state, &accepted, "ada@example.com").await;

    let (status, body) = invite_as(&state, &admin, "bob@example.com").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["delivery"], "sent",
        "the message really did leave, and reporting otherwise would be a lie: {body}"
    );
    assert!(
        body["notifiedAtMillis"].is_null(),
        "an invite revoked mid-send has nothing left to stamp: {body}"
    );

    // The property this test exists for. The stamp is written from a record
    // read before the send, so an upsert would put the revoked invite back —
    // silently returning an address to the allowlist after an admin removed it,
    // with nothing on screen to say so.
    let app = router(state.clone());
    let response = app
        .oneshot(get_with_cookie(
            "/api/v1/companies/acme/users/invites",
            &admin,
        ))
        .await
        .unwrap();
    let invites = body_json(response).await;
    assert!(
        !invites
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["email"] == "bob@example.com"),
        "the mailed stamp must not restore a revoked invite: {invites}"
    );
}

#[tokio::test]
async fn a_refused_invite_mails_nobody() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let (state, sender) = state_with_mail(&home).await;
    let admin = login_via_link(&state, &sender, "ada@example.com").await;

    // Already a member: Ada bootstraps from the manifest and has signed in.
    let before = sender.sent().len();
    let (status, _) = invite_as(&state, &admin, "ada@example.com").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        sender.sent().len(),
        before,
        "an invite refused as already-a-member must mail nobody"
    );

    // And a duplicate outstanding invite is refused by the store, also silently
    // as far as the mailbox is concerned — one invitation per address, not one
    // per click. Without this, the button is a mail cannon aimed at whoever an
    // admin most recently typed.
    let (status, _) = invite_as(&state, &admin, "bob@example.com").await;
    assert_eq!(status, StatusCode::OK);
    let after_first = sender.sent().len();

    let (status, _) = invite_as(&state, &admin, "bob@example.com").await;
    assert_eq!(status, StatusCode::CONFLICT, "one invite per address");
    assert_eq!(
        sender.sent().len(),
        after_first,
        "a duplicate invite must not re-mail"
    );

    // A malformed login never reaches a transport either.
    let (status, _) = invite_as(&state, &admin, "not a login").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        sender.sent().len(),
        after_first,
        "a rejected address must mail nobody"
    );
}

/// A person names themselves and picks a face, without an admin in the loop.
/// The whole reason this route exists beside the admin one: your own identity
/// in a company should not be something you have to ask for.
#[tokio::test]
async fn a_person_can_name_themselves_and_pick_a_face() {
    let home = home();
    let (state, sender) = state_with_mail(home.path()).await;
    let cookie = login_via_link(&state, &sender, "ada@example.com").await;

    let (status, me) = patch_me(
        &state,
        &cookie,
        serde_json::json!({"displayName": "Ada L.", "avatar": "tiny:violet"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["displayName"], "Ada L.", "{me}");
    assert_eq!(me["avatar"], "tiny:violet", "{me}");

    // Persisted, not just echoed.
    let response = router(state.clone())
        .oneshot(get_with_cookie("/api/v1/companies/acme/auth/me", &cookie))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let reread = body_json(response).await;
    assert_eq!(reread["displayName"], "Ada L.", "{reread}");
    assert_eq!(reread["avatar"], "tiny:violet", "{reread}");
}

/// A partial save leaves the field it did not mention alone — the reason both
/// fields are double options. Without it, saving a name wipes the face.
#[tokio::test]
async fn editing_one_field_of_a_profile_leaves_the_other() {
    let home = home();
    let (state, sender) = state_with_mail(home.path()).await;
    let cookie = login_via_link(&state, &sender, "ada@example.com").await;

    patch_me(&state, &cookie, serde_json::json!({"avatar": "tiny:rose"})).await;
    let (_, named) = patch_me(&state, &cookie, serde_json::json!({"displayName": "Ada"})).await;
    assert_eq!(named["avatar"], "tiny:rose", "{named}");

    // And each is individually resettable: `null` — or a blanked input, which is
    // the same intent typed — goes back to the default.
    let (_, unnamed) = patch_me(&state, &cookie, serde_json::json!({"displayName": "  "})).await;
    assert!(
        unnamed.get("displayName").is_none(),
        "a blank name is not a name: {unnamed}"
    );
    assert_eq!(unnamed["avatar"], "tiny:rose", "{unnamed}");
    let (_, bare) = patch_me(&state, &cookie, serde_json::json!({"avatar": null})).await;
    assert!(
        bare.get("avatar").is_none(),
        "a reset is absent, not empty: {bare}"
    );
}

/// The grammar's rule, on this route too: an avatar names something this host
/// holds, never a URL the console would fetch on this person's behalf.
#[tokio::test]
async fn a_profile_avatar_may_not_be_a_url() {
    let home = home();
    let (state, sender) = state_with_mail(home.path()).await;
    let cookie = login_via_link(&state, &sender, "ada@example.com").await;

    for hostile in [
        "https://tracker.example/beacon.gif",
        "javascript:alert(1)",
        "blob:01NOSUCHNODE",
    ] {
        let (status, refused) =
            patch_me(&state, &cookie, serde_json::json!({"avatar": hostile})).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{hostile} was accepted: {refused}"
        );
    }
}

/// A display name is a bounded field: it renders on every surface that shows a
/// person and rides in every roster payload, so a page of text parked in it
/// would be served to everyone. The bound is a `400`, not a truncation.
#[tokio::test]
async fn a_profile_name_may_not_exceed_the_bound() {
    let home = home();
    let (state, sender) = state_with_mail(home.path()).await;
    let cookie = login_via_link(&state, &sender, "ada@example.com").await;

    let long = "A".repeat(crate::server::users::MAX_DISPLAY_NAME_CHARS + 1);
    let (status, refused) =
        patch_me(&state, &cookie, serde_json::json!({"displayName": long})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");

    // Nothing was persisted, and a subsequent normal save still works.
    let (status, _) = patch_me(
        &state,
        &cookie,
        serde_json::json!({"displayName": "Ada L."}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// No session, no profile. There is no `user_id` in the path to point at
/// somebody else, so this is the whole of the route's authority check.
#[tokio::test]
async fn a_profile_edit_needs_a_session() {
    let home = home();
    let (state, _sender) = state_with_mail(home.path()).await;
    let response = router(state.clone())
        .oneshot(patch_with_cookie(
            "/api/v1/companies/acme/auth/me",
            serde_json::json!({"displayName": "Nobody"}),
            "oc_session_acme=not-a-session",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// The loser of a race adopts the winner rather than being refused.
///
/// Deterministic where the race itself is not: it stages the exact state a
/// losing caller finds itself in — an address already held, by an id that is not
/// the one this caller minted — and asserts the outcome is the winner's record
/// rather than a `Conflict`.
///
/// Before #1833 this returned `Err(Conflict)`, `graphql::auth` turned that into
/// `GatesRefused`, and the desktop console reported the healthy host it was
/// talking to as "Unreachable".
#[tokio::test]
async fn a_lost_materialization_race_adopts_the_winner() {
    use crate::ports::users::{UserRecord, UserRole, UserStatus};

    let home = home();
    let runtime = users_runtime(home.path()).await;
    let id = runtime.id();
    let email = crate::ports::users::LoginIdentity::Local.key();

    let winner = UserRecord {
        id: "winner-id".to_string(),
        email: email.clone(),
        display_name: None,
        avatar: None,
        role: UserRole::Admin,
        status: UserStatus::Active,
        password_hash: None,
        must_change_password: false,
        created_at_millis: 1,
        last_seen_at_millis: Some(1),
        updated_at_millis: 1,
    };
    runtime.users().upsert_user(id, &winner).await.unwrap();

    // The loser: same address, its own freshly generated id — which is exactly
    // what makes the store refuse it.
    let loser = UserRecord {
        id: "loser-id".to_string(),
        created_at_millis: 2,
        ..winner.clone()
    };

    let adopted = crate::server::users::routes::insert_or_adopt(&runtime, loser)
        .await
        .expect("a lost race is not an error — the owner exists");

    assert_eq!(
        adopted.id, "winner-id",
        "the loser must return the record that won, not its own"
    );

    // And the store still holds exactly one owner: adopting must not have
    // written the loser's id alongside the winner's.
    let held = runtime.users().list_users(id).await.unwrap();
    let owners: Vec<_> = held.iter().filter(|u| u.email == email).collect();
    assert_eq!(owners.len(), 1, "exactly one owner record: {held:?}");
    assert_eq!(owners[0].id, "winner-id");
}

/// Concurrent callers converge on one owner.
///
/// The shape of the original bug: N requests arrive together on a cold store,
/// all miss `find_user_by_email`, and each presents a different `generate_id()`
/// for one address. On the desktop's first boot three of them raced; one won and
/// two were refused 16ms later.
///
/// Timing-dependent by nature — it cannot *guarantee* an interleaving — so it is
/// the companion to the deterministic test above rather than the proof. What it
/// does catch is a regression that reintroduces the shape, and it fails reliably
/// against the pre-#1833 code at this width.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_local_owner_materialization_yields_one_record() {
    let home = home();
    let runtime = users_runtime(home.path()).await;

    let racers = 16;
    let mut tasks = Vec::with_capacity(racers);
    for _ in 0..racers {
        let runtime = Arc::clone(&runtime);
        tasks.push(tokio::spawn(async move {
            crate::server::users::routes::local_owner_record(&runtime).await
        }));
    }

    let mut ids = Vec::with_capacity(racers);
    for task in tasks {
        let record = task
            .await
            .expect("no racer panics")
            .expect("no racer is refused its own company's owner");
        ids.push(record.id);
    }

    let first = &ids[0];
    assert!(
        ids.iter().all(|id| id == first),
        "every racer must see one owner, got {ids:?}"
    );

    let held = runtime.users().list_users(runtime.id()).await.unwrap();
    let key = crate::ports::users::LoginIdentity::Local.key();
    assert_eq!(
        held.iter().filter(|u| u.email == key).count(),
        1,
        "exactly one owner record survives the race: {held:?}"
    );
}
