use crate::app::config::AuthMode;
use crate::ports::CompanyStore;
use crate::ports::types::CompanyId;
use crate::server::ops::ConnectionsRuntime;
use crate::server::router;
use crate::server::users::token;
use crate::server::users::wallet::{self, VerifyRequest};
use axum::http::StatusCode;
use ed25519_dalek::Signer as _;
use tower::ServiceExt;

use super::mode_test_support_1::*;

// ---------------------------------------------------------------------------
// What the console is told
// ---------------------------------------------------------------------------

/// The console cannot draw a sign-in screen without this, and it must be able to
/// ask before it has any credential.
#[tokio::test]
async fn auth_config_publishes_the_mode_to_an_anonymous_caller() {
    for (mode, passwords) in [
        (AuthMode::Email, true),
        (AuthMode::Wallet, false),
        (AuthMode::None, false),
    ] {
        let dir = home();
        let state = state_in_mode(dir.path(), mode, None).await;
        let response = router(state)
            .oneshot(get("/api/v1/company/auth/config"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{mode}");
        let body = body_json(response).await;
        assert_eq!(body["mode"], mode.as_str(), "{mode}");
        assert_eq!(body["passwords"], passwords, "{mode}");
    }
}

/// The sign-in screen is the one place a person confirms *what* they are
/// signing in to before handing over a credential, and on the hosted platform
/// every tenant is a separate company on its own URL. The console cannot ask
/// anything else for the name — every other route that reports it is behind the
/// very sign-in being drawn — so it has to come back here (issue #1334).
#[tokio::test]
async fn auth_config_names_the_company_to_an_anonymous_caller() {
    for mode in [AuthMode::Email, AuthMode::Wallet, AuthMode::None] {
        let dir = home();
        let state = state_in_mode(dir.path(), mode, None).await;
        let response = router(state)
            .oneshot(get("/api/v1/company/auth/config"))
            .await
            .unwrap();
        let body = body_json(response).await;
        // The manifest's display name, not the id it is stored under — the
        // fixture spells them differently ("Acme" vs `acme`) precisely so a
        // fallback to the id cannot pass this.
        assert_eq!(
            body["name"], "Acme",
            "every mode draws a heading, so every mode needs the name: {body}"
        );
    }
}

/// A manifest that names the company nothing still has to produce a heading.
/// The id is what every other surface calls it in that case — `status` makes
/// the same substitution — and a blank `h1` is the bug this field exists to
/// remove, so it must not be reachable by writing `name = ""`.
#[tokio::test]
async fn auth_config_falls_back_to_the_company_id_when_the_manifest_has_no_name() {
    let dir = home();
    let state = state_in_mode(dir.path(), AuthMode::Email, None).await;
    let id = CompanyId::new("acme");
    let store = crate::store::FsCompanyStore::new(dir.path().to_path_buf());
    let mut record = store.load(&id).await.unwrap().expect("the fixture record");
    record.manifest.company.name = "   ".to_string();
    store.save(&record).await.unwrap();

    let response = router(state)
        .oneshot(get("/api/v1/company/auth/config"))
        .await
        .unwrap();
    let body = body_json(response).await;
    assert_eq!(
        body["name"], "acme",
        "a blank name is not a heading; the id is: {body}"
    );
}

/// The record the name comes from is not there, or is not readable.
///
/// Both are the same statement: a heading is decoration on a route whose real
/// payload is the mode, and a console that cannot learn the mode draws the
/// wrong screen entirely. So neither case may fail the request — they fall back
/// to the id, exactly as a blank name does.
///
/// The two are separate paths in `display_name`: a missing bundle is `Ok(None)`
/// from the store, an unreadable one is `Err`, and the `Err` arm is the one that
/// would take the route down if it were propagated.
#[tokio::test]
async fn auth_config_falls_back_to_the_company_id_when_the_record_cannot_be_read() {
    for (case, contents) in [("gone", None), ("unreadable", Some("}} not toml {{"))] {
        let dir = home();
        let state = state_in_mode(dir.path(), AuthMode::Email, None).await;
        let manifest_path =
            crate::store::paths::Bundle::new(dir.path(), &CompanyId::new("acme")).company_toml();
        match contents {
            Some(garbage) => std::fs::write(&manifest_path, garbage).unwrap(),
            None => std::fs::remove_file(&manifest_path).unwrap(),
        }

        let response = router(state)
            .oneshot(get("/api/v1/company/auth/config"))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a missing name must not cost the console the mode ({case})"
        );
        let body = body_json(response).await;
        assert_eq!(body["name"], "acme", "({case}) {body}");
        assert_eq!(body["mode"], "email", "({case}) {body}");
    }
}

/// A routable host with no transport cannot deliver a magic link and will not
/// echo the code either, so the form is a dead end. The console has to be told
/// that in the payload — from the outside a link request there answers `sent`
/// exactly like one that worked.
#[tokio::test]
async fn auth_config_reports_a_magic_link_that_cannot_arrive() {
    let dir = home();
    let state = state_in_mode_on(
        dir.path(),
        AuthMode::Email,
        None,
        routable(),
        ConnectionsRuntime::new(),
    )
    .await;
    let response = router(state)
        .oneshot(get("/api/v1/company/auth/config"))
        .await
        .unwrap();
    let body = body_json(response).await;

    assert_eq!(
        body["magicLink"], false,
        "no transport and no echo is a dead end: {body}"
    );
}

/// Only a wired transport makes the link real. A loopback host with no
/// transport still echoes the code on the API for tooling, but the sign-in
/// screen must not offer "email me a link" on a host that will email nothing:
/// the password is the way in there, and the screen says so.
#[tokio::test]
async fn auth_config_reports_a_magic_link_only_where_mail_is_wired() {
    let dir = home();
    let mailed = state_in_mode_on(
        dir.path(),
        AuthMode::Email,
        None,
        routable(),
        mail_connections(),
    )
    .await;
    let response = router(mailed)
        .oneshot(get("/api/v1/company/auth/config"))
        .await
        .unwrap();
    let body = body_json(response).await;
    assert_eq!(
        body["magicLink"], true,
        "a wired transport sends it: {body}"
    );

    let echoed = home();
    let loopback = state_in_mode(echoed.path(), AuthMode::Email, None).await;
    let response = router(loopback)
        .oneshot(get("/api/v1/company/auth/config"))
        .await
        .unwrap();
    let body = body_json(response).await;
    assert_eq!(
        body["magicLink"], false,
        "a loopback host with no transport offers no link form: {body}"
    );
}

/// `claimable` is the sign-in screen's cue to offer the first admin claim. It
/// is true on an email company nobody has joined, and never in another mode —
/// a wallet company bootstraps from its key list and a `none` company has no
/// accounts at all.
#[tokio::test]
async fn auth_config_reports_claimable_only_on_an_empty_email_company() {
    for (mode, expected) in [
        (AuthMode::Email, true),
        (AuthMode::Wallet, false),
        (AuthMode::None, false),
    ] {
        let dir = home();
        let state = state_in_mode(dir.path(), mode, None).await;
        let response = router(state)
            .oneshot(get("/api/v1/company/auth/config"))
            .await
            .unwrap();
        let body = body_json(response).await;
        assert_eq!(body["claimable"], expected, "({}) {body}", mode.as_str());
    }
}

// ---------------------------------------------------------------------------
// One mode, one door
// ---------------------------------------------------------------------------

/// A wallet company has no magic link, no password login, and no hub buttons.
/// Each would be a second way onto the roster that nobody configured.
#[tokio::test]
async fn a_wallet_company_refuses_every_email_route() {
    let dir = home();
    let state = state_in_mode(dir.path(), AuthMode::Wallet, None).await;
    let app = router(state);

    for request in [
        post(
            "/api/v1/company/auth/request",
            serde_json::json!({"email": "ada@example.com"}),
        ),
        post(
            "/api/v1/company/auth/verify",
            serde_json::json!({"code": "x"}),
        ),
        post(
            "/api/v1/company/auth/login",
            serde_json::json!({"email": "ada@example.com", "password": "hunter2hunter2"}),
        ),
    ] {
        let uri = request.uri().to_string();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT, "{uri}");
        let body = body_json(response).await;
        assert_eq!(body["code"], "auth_mode", "{uri}");
        // The refusal names the mode, so a console that got here can correct
        // itself rather than telling somebody their address was wrong.
        assert_eq!(body["mode"], "wallet", "{uri}");
    }
}

/// An email company has no wallet door.
#[tokio::test]
async fn an_email_company_refuses_the_wallet_routes() {
    let dir = home();
    let state = state_in_mode(dir.path(), AuthMode::Email, None).await;
    let response = router(state)
        .oneshot(post(
            "/api/v1/company/auth/wallet/challenge",
            serde_json::json!({"address": address(&wallet(1))}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(response).await["mode"], "email");
}

// ---------------------------------------------------------------------------
// Wallet sign-in
// ---------------------------------------------------------------------------

/// The whole flow: challenge, sign, session. The signature is produced by a real
/// Ed25519 key over the exact bytes the host handed back, which is what a
/// browser wallet does.
#[tokio::test]
async fn a_bootstrapped_wallet_signs_in() {
    let dir = home();
    let key = wallet(3);
    let addr = address(&key);
    let app = router(state_in_mode(dir.path(), AuthMode::Wallet, Some(&addr)).await);

    let challenge = body_json(
        app.clone()
            .oneshot(post(
                "/api/v1/company/auth/wallet/challenge",
                serde_json::json!({"address": addr}),
            ))
            .await
            .unwrap(),
    )
    .await;
    let message = challenge["message"].as_str().unwrap();
    // The layout is the host's and is versioned by its first line; a console
    // must sign it verbatim rather than rebuilding it.
    assert!(
        message.starts_with("opencompany-wallet-login-v1\nacme\n"),
        "{message}"
    );
    assert!(message.contains(&addr), "the address is bound: {message}");

    let signature = bs58::encode(key.sign(message.as_bytes()).to_bytes()).into_string();
    let response = app
        .oneshot(post(
            "/api/v1/company/auth/wallet/verify",
            serde_json::json!({"nonce": challenge["nonce"], "signature": signature}),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = response
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        set_cookie.contains("oc_session_acme=") && set_cookie.contains("HttpOnly"),
        "a wallet sign-in mints the same ordinary session a link does: {set_cookie}"
    );
    let me = body_json(response).await;
    // The identity is stored scheme-prefixed, so it can never collide with an
    // email in the same column.
    assert_eq!(me["email"], format!("wallet:{addr}"));
    assert_eq!(me["role"], "admin");
}

/// A nonce is good exactly once. The store consumes it atomically, so a captured
/// request cannot be replayed.
#[tokio::test]
async fn a_challenge_cannot_be_answered_twice() {
    let dir = home();
    let key = wallet(4);
    let addr = address(&key);
    let app = router(state_in_mode(dir.path(), AuthMode::Wallet, Some(&addr)).await);

    let challenge = body_json(
        app.clone()
            .oneshot(post(
                "/api/v1/company/auth/wallet/challenge",
                serde_json::json!({"address": addr}),
            ))
            .await
            .unwrap(),
    )
    .await;
    let signature = bs58::encode(
        key.sign(challenge["message"].as_str().unwrap().as_bytes())
            .to_bytes(),
    )
    .into_string();
    let answer = serde_json::json!({"nonce": challenge["nonce"], "signature": signature});

    let first = app
        .clone()
        .oneshot(post("/api/v1/company/auth/wallet/verify", answer.clone()))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let replay = app
        .oneshot(post("/api/v1/company/auth/wallet/verify", answer))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(replay).await["code"], "invalid_login");
}

/// Requesting a second challenge for the same wallet invalidates the first: the
/// route must not accumulate one durable `LoginCodeRecord` per request, which
/// would let an unauthenticated caller who keeps naming an eligible wallet grow
/// the challenge table without bound.
#[tokio::test]
async fn a_second_challenge_inside_the_throttle_window_does_not_invalidate_the_first() {
    let dir = home();
    let key = wallet(9);
    let addr = address(&key);
    let app = router(state_in_mode(dir.path(), AuthMode::Wallet, Some(&addr)).await);

    let first = body_json(
        app.clone()
            .oneshot(post(
                "/api/v1/company/auth/wallet/challenge",
                serde_json::json!({"address": addr}),
            ))
            .await
            .unwrap(),
    )
    .await;
    let second = body_json(
        app.clone()
            .oneshot(post(
                "/api/v1/company/auth/wallet/challenge",
                serde_json::json!({"address": addr}),
            ))
            .await
            .unwrap(),
    )
    .await;

    // The second request landed inside the throttle window, so it answered
    // with a decoy rather than replacing the pending challenge — the first
    // nonce is still exactly what the owner should sign.
    let first_signature = bs58::encode(
        key.sign(first["message"].as_str().unwrap().as_bytes())
            .to_bytes(),
    )
    .into_string();
    let answered = app
        .clone()
        .oneshot(post(
            "/api/v1/company/auth/wallet/verify",
            serde_json::json!({"nonce": first["nonce"], "signature": first_signature}),
        ))
        .await
        .unwrap();
    assert_eq!(
        answered.status(),
        StatusCode::OK,
        "a throttled replacement must not invalidate the pending challenge"
    );

    // The decoy the second request returned is not a real challenge — it was
    // never persisted — so signing it answers nothing.
    let decoy_signature = bs58::encode(
        key.sign(second["message"].as_str().unwrap().as_bytes())
            .to_bytes(),
    )
    .into_string();
    let decoy_answer = app
        .oneshot(post(
            "/api/v1/company/auth/wallet/verify",
            serde_json::json!({"nonce": second["nonce"], "signature": decoy_signature}),
        ))
        .await
        .unwrap();
    assert_eq!(decoy_answer.status(), StatusCode::UNAUTHORIZED);
}

/// Once the throttle window has passed, a replacement challenge really does
/// invalidate the one it replaces — the throttle is a delay, not a
/// prohibition, so the roster's own admin still gets a working "one live
/// challenge" invariant once the window clears.
#[tokio::test]
async fn a_challenge_replaces_the_previous_one_once_the_throttle_window_passes() {
    let dir = home();
    let key = wallet(14);
    let addr = address(&key);
    let state = state_in_mode(dir.path(), AuthMode::Wallet, Some(&addr)).await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();

    let t0 = 1_000_000_u64;
    let first = wallet::issue_challenge(&runtime, &token::OsTokens, &addr, t0)
        .await
        .unwrap();
    let t1 = t0 + wallet::CHALLENGE_RESEND_INTERVAL_MILLIS + 1;
    let second = wallet::issue_challenge(&runtime, &token::OsTokens, &addr, t1)
        .await
        .unwrap();
    assert_ne!(first.nonce, second.nonce);

    let stale_body = VerifyRequest {
        nonce: first.nonce,
        signature: bs58::encode(key.sign(first.message.as_bytes()).to_bytes()).into_string(),
    };
    assert!(
        wallet::verify_challenge(&runtime, &stale_body, t1)
            .await
            .is_none(),
        "the replaced challenge must no longer redeem"
    );

    let fresh_body = VerifyRequest {
        nonce: second.nonce,
        signature: bs58::encode(key.sign(second.message.as_bytes()).to_bytes()).into_string(),
    };
    assert!(
        wallet::verify_challenge(&runtime, &fresh_body, t1)
            .await
            .is_some()
    );
}

/// Inviting a wallet identity has no mailbox to write to, and the invite route
/// must say so — `no_mailbox`, not `no_transport` or a silent `sent` — since the
/// console renders this delivery status for an admin who typed the address in.
#[tokio::test]
async fn inviting_a_wallet_reports_no_mailbox_delivery() {
    let dir = home();
    let admin_key = wallet(11);
    let admin_addr = address(&admin_key);
    let app = router(state_in_mode(dir.path(), AuthMode::Wallet, Some(&admin_addr)).await);

    let challenge = body_json(
        app.clone()
            .oneshot(post(
                "/api/v1/company/auth/wallet/challenge",
                serde_json::json!({"address": admin_addr}),
            ))
            .await
            .unwrap(),
    )
    .await;
    let signature = bs58::encode(
        admin_key
            .sign(challenge["message"].as_str().unwrap().as_bytes())
            .to_bytes(),
    )
    .into_string();
    let verify = app
        .clone()
        .oneshot(post(
            "/api/v1/company/auth/wallet/verify",
            serde_json::json!({"nonce": challenge["nonce"], "signature": signature}),
        ))
        .await
        .unwrap();
    assert_eq!(verify.status(), StatusCode::OK);
    let cookie = session_cookie(&verify);

    let invitee_addr = address(&wallet(12));
    let response = app
        .oneshot(post_with_cookie(
            "/api/v1/company/users/invites",
            serde_json::json!({"wallet": invitee_addr}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(
        body["delivery"], "no_mailbox",
        "a wallet invite has no mailbox to write to: {body}"
    );
    assert_eq!(body["email"], format!("wallet:{invitee_addr}"));
}

/// An invite naming the field the company's mode does not read is refused
/// rather than silently ignored — an admin who fills in the wrong field, or
/// both, must not believe they invited something they did not.
#[tokio::test]
async fn inviting_with_the_wrong_identity_field_is_refused() {
    let dir = home();
    let admin_key = wallet(13);
    let admin_addr = address(&admin_key);
    let app = router(state_in_mode(dir.path(), AuthMode::Wallet, Some(&admin_addr)).await);

    let challenge = body_json(
        app.clone()
            .oneshot(post(
                "/api/v1/company/auth/wallet/challenge",
                serde_json::json!({"address": admin_addr}),
            ))
            .await
            .unwrap(),
    )
    .await;
    let signature = bs58::encode(
        admin_key
            .sign(challenge["message"].as_str().unwrap().as_bytes())
            .to_bytes(),
    )
    .into_string();
    let verify = app
        .clone()
        .oneshot(post(
            "/api/v1/company/auth/wallet/verify",
            serde_json::json!({"nonce": challenge["nonce"], "signature": signature}),
        ))
        .await
        .unwrap();
    let cookie = session_cookie(&verify);

    // An `email` field on a wallet company is refused, whether or not `wallet`
    // is also set.
    let response = app
        .oneshot(post_with_cookie(
            "/api/v1/company/users/invites",
            serde_json::json!({"email": "bob@example.com"}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// A wallet that is not on the roster gets a challenge shaped exactly like a
/// real one — the route must not be a membership oracle — and it verifies as
/// nothing.
#[tokio::test]
async fn an_uninvited_wallet_gets_a_challenge_that_does_not_work() {
    let dir = home();
    let invited = wallet(5);
    let stranger = wallet(6);
    let app = router(state_in_mode(dir.path(), AuthMode::Wallet, Some(&address(&invited))).await);

    let challenge = body_json(
        app.clone()
            .oneshot(post(
                "/api/v1/company/auth/wallet/challenge",
                serde_json::json!({"address": address(&stranger)}),
            ))
            .await
            .unwrap(),
    )
    .await;
    // Indistinguishable from an invited wallet's challenge.
    assert!(challenge["nonce"].as_str().is_some_and(|n| !n.is_empty()));
    assert!(
        challenge["message"]
            .as_str()
            .unwrap()
            .contains(&address(&stranger))
    );

    let signature = bs58::encode(
        stranger
            .sign(challenge["message"].as_str().unwrap().as_bytes())
            .to_bytes(),
    )
    .into_string();
    let response = app
        .oneshot(post(
            "/api/v1/company/auth/wallet/verify",
            serde_json::json!({"nonce": challenge["nonce"], "signature": signature}),
        ))
        .await
        .unwrap();
    // The same failure a forged signature gets. Nothing distinguishes "not on
    // the roster" from "that is not your key".
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(response).await["code"], "invalid_login");
}

/// The address is taken from the stored challenge, never from the request, so a
/// wallet cannot answer a challenge issued to another one.
#[tokio::test]
async fn another_wallets_signature_does_not_answer_the_challenge() {
    let dir = home();
    let invited = wallet(7);
    let impostor = wallet(8);
    let addr = address(&invited);
    let app = router(state_in_mode(dir.path(), AuthMode::Wallet, Some(&addr)).await);

    let challenge = body_json(
        app.clone()
            .oneshot(post(
                "/api/v1/company/auth/wallet/challenge",
                serde_json::json!({"address": addr}),
            ))
            .await
            .unwrap(),
    )
    .await;
    // A perfectly valid signature — over the right bytes, by the wrong key.
    let signature = bs58::encode(
        impostor
            .sign(challenge["message"].as_str().unwrap().as_bytes())
            .to_bytes(),
    )
    .into_string();

    let response = app
        .oneshot(post(
            "/api/v1/company/auth/wallet/verify",
            serde_json::json!({"nonce": challenge["nonce"], "signature": signature}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// No sign-in at all
// ---------------------------------------------------------------------------

/// The point of the mode: a request carrying no credential is the owner. If this
/// failed, `none` would not be "no sign-in", it would be a console nobody can
/// use — and the two look identical from outside.
#[tokio::test]
async fn none_mode_serves_the_local_owner_with_no_credential() {
    let dir = home();
    let state = state_in_mode(dir.path(), AuthMode::None, None).await;
    let response = router(state)
        .oneshot(get("/api/v1/company/auth/me"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let me = body_json(response).await;
    assert_eq!(me["email"], "local:owner");
    // The person at the machine owns the company; there is nobody for a lesser
    // role to be distinguished from.
    assert_eq!(me["role"], "admin");
}
