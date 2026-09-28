//! HTTP-level tests for the ops write plane (domain, SMTP, inbox ingest).
//!
//! Every networked seam is exercised offline through injected mocks: a
//! [`StaticDnsResolver`](crate::company::dns::StaticDnsResolver) for domain
//! verify and a [`RecordingMailSender`](super::smtp::RecordingMailSender) for
//! the SMTP test send. The default build links no network crate.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::CompanyStore;
use crate::ports::types::{CompanyId, CompanyRecord, SecretValue};
use crate::runtime::RuntimeBuilder;
use crate::server::ops::ConnectionsRuntime;
use crate::server::ops::mailer::RecordingMailSender;
use crate::server::router;
use crate::{AppConfig, AppState};

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("opencompany-ops-")
        .tempdir()
        .expect("tempdir")
}

fn manifest() -> CompanyManifest {
    toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap()
}

/// Builds state holding one running company `acme`, with `connections` injected.
async fn state_with(home: &std::path::Path, connections: ConnectionsRuntime) -> AppState {
    state_with_secrets(home, connections, None).await
}

/// [`state_with`], optionally over an injected
/// [`SecretStore`](crate::ports::SecretStore).
///
/// The seam exists for the tests that have to observe *when* the SMTP secrets
/// are read and written, which the on-disk store gives no way to see.
async fn state_with_secrets(
    home: &std::path::Path,
    connections: ConnectionsRuntime,
    secrets: Option<Arc<dyn crate::ports::SecretStore>>,
) -> AppState {
    let store = crate::store::FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
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
        })
        .await
        .unwrap();
    let mut builder = RuntimeBuilder::new(home.to_path_buf(), manifest()).with_id(id.clone());
    if let Some(secrets) = secrets {
        builder = builder.with_secrets(secrets);
    }
    let runtime = builder.build().await.unwrap();
    let state = AppState::new(AppConfig::default())
        .with_home(home.to_path_buf())
        .with_connections(connections);
    state.registry().insert(id, Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn put_smtp_without_a_password_and_nothing_stored_is_refused() {
    // The other end of the patch: keeping "the stored password" only works when
    // there is one. Accepted, it would store credentials that can never
    // authenticate while the settings page read "configured".
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with(&home, ConnectionsRuntime::new()).await;
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/smtp")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"host":"smtp.acme.test","port":587,"username":"mailer","from_email":"ceo@acme.test"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let value = body_json(response).await;
    assert_eq!(value["code"], "invalid_request", "{value}");
}

#[tokio::test]
async fn a_password_keeps_its_leading_and_trailing_spaces() {
    // `str::trim` on the way in would store `" pad ded "` as `"pad ded"`, and
    // the operator would watch authentication fail against a password they can
    // see is correct, with nothing in any response or log naming the edit. SMTP
    // passwords are opaque bytes; only the caller knows where they end.
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let sender = Arc::new(RecordingMailSender::new());
    let state = state_with(&home, ConnectionsRuntime::new().with_mail(sender.clone())).await;
    let app = router(state);

    // Deliberately fake, and deliberately padded at both ends.
    let padded = "  pad ded  ";
    let body = serde_json::json!({
        "host": "smtp.acme.test",
        "port": 587,
        "security": "starttls",
        "username": "mailer",
        "password": padded,
        "from_name": "Acme",
        "from_email": "ceo@acme.test",
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/smtp")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // What reached the transport is the only observable answer — the password is
    // write-only, so no read can report it back.
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/smtp/test")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let presented = sender.presented();
    assert_eq!(presented.len(), 1);
    let crate::server::ops::mailer::MailCredentials::Smtp(creds) = &presented[0];
    assert_eq!(
        creds.password.expose(),
        padded,
        "the stored password must be byte-for-byte what was supplied",
    );
}

/// A secret store that lets one "concurrent rotation" land at a chosen moment.
///
/// The rotation commits immediately *before* this request's first write, which
/// is the only interleaving that can lose it: the other admin's `PUT …/smtp`
/// has landed after this request read whatever it read, so anything this
/// request now writes is written on top of the rotation. Hanging it off the
/// write rather than the read is deliberate — a request may read several times,
/// and firing on the first read would let a later read observe the rotation and
/// quietly launder the bug into a pass.
///
/// It is applied to both places a password can live — its own key and the
/// pre-split configuration blob — so it is a genuine rotation under either
/// storage layout, and the test can ask the one question that matters: does the
/// save that follows revert it?
#[derive(Default)]
struct RotatingSecrets {
    entries: std::sync::Mutex<std::collections::HashMap<String, String>>,
    /// `(password, blob)` to commit just before the next SMTP write, once.
    rotation: std::sync::Mutex<Option<(String, String)>>,
    /// Milliseconds to stall inside a read of the configuration blob.
    ///
    /// Holds the read-modify-write window of the legacy migration open long
    /// enough for a rotation to try to slip into it. Without this the two
    /// requests finish too quickly to interleave, and the test would pass just
    /// as happily with no serialization at all.
    stall_config_read_ms: std::sync::atomic::AtomicU64,
}

impl RotatingSecrets {
    /// Stalls every read of the configuration blob by `ms`.
    fn stall_config_reads(&self, ms: u64) {
        self.stall_config_read_ms
            .store(ms, std::sync::atomic::Ordering::SeqCst);
    }

    /// Arms the one-shot rotation.
    fn arm(&self, password: &str, blob: &str) {
        *self.rotation.lock().unwrap() = Some((password.to_string(), blob.to_string()));
    }

    /// The password an ordinary read would now resolve to.
    fn stored_password(&self) -> Option<String> {
        self.entries
            .lock()
            .unwrap()
            .get(super::SMTP_PASSWORD_KEY)
            .cloned()
    }
}

#[async_trait::async_trait]
impl crate::ports::SecretStore for RotatingSecrets {
    async fn get(
        &self,
        _company: &CompanyId,
        key: &str,
    ) -> crate::Result<Option<crate::ports::types::SecretValue>> {
        let seen = self.entries.lock().unwrap().get(key).cloned();
        let stall = self
            .stall_config_read_ms
            .load(std::sync::atomic::Ordering::SeqCst);
        if key == super::SMTP_KEY && stall > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(stall)).await;
        }
        Ok(seen.map(SecretValue))
    }

    async fn set(&self, _company: &CompanyId, key: &str, value: SecretValue) -> crate::Result<()> {
        // The other admin gets in first; this request's write lands on top of
        // theirs, which is what makes a lost rotation observable.
        if matches!(key, super::SMTP_KEY | super::SMTP_PASSWORD_KEY)
            && let Some((password, blob)) = self.rotation.lock().unwrap().take()
        {
            let mut entries = self.entries.lock().unwrap();
            entries.insert(super::SMTP_PASSWORD_KEY.to_string(), password);
            entries.insert(super::SMTP_KEY.to_string(), blob);
        }
        self.entries
            .lock()
            .unwrap()
            .insert(key.to_string(), value.expose().to_string());
        Ok(())
    }
}

#[tokio::test]
async fn a_passwordless_save_does_not_revert_a_concurrent_rotation() {
    // Two admins, overlapping requests. One is correcting the display name and
    // sends no password; the other is rotating the credential. If the first
    // request keeps the password by loading it and writing it back, it reverts
    // the rotation it never knew about, and nothing anywhere reports that the
    // company is now authenticating with a retired secret.
    //
    // The fix is structural rather than a lock: the password lives under its own
    // key, so "keep the stored password" is the absence of a write.
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let secrets = Arc::new(RotatingSecrets::default());
    let sender = Arc::new(RecordingMailSender::new());
    let state = state_with_secrets(
        &home,
        ConnectionsRuntime::new().with_mail(sender.clone()),
        Some(secrets.clone()),
    )
    .await;
    let app = router(state);

    // Fake throughout; these never leave the test process.
    let original = "original-pw";
    let rotated = "rotated-pw";

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/smtp")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "host": "smtp.acme.test",
                        "port": 587,
                        "security": "starttls",
                        "username": "mailer",
                        "password": original,
                        "from_name": "Acme",
                        "from_email": "ceo@acme.test",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(secrets.stored_password().as_deref(), Some(original));

    // The other admin's rotation, committing mid-request. The blob carries the
    // rotated password too, so this is a real rotation even for a reader that
    // still expects the password to live inside it.
    secrets.arm(
        rotated,
        &serde_json::json!({
            "host": "smtp.acme.test",
            "port": 587,
            "security": "starttls",
            "username": "mailer",
            "password": rotated,
            "from_name": "Acme",
            "from_email": "ceo@acme.test",
        })
        .to_string(),
    );

    // The display-name correction: same body, no password.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/smtp")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "host": "smtp.acme.test",
                        "port": 587,
                        "security": "starttls",
                        "username": "mailer",
                        "from_name": "Acme Inc",
                        "from_email": "ceo@acme.test",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // The rotation stands, and the display name still took effect.
    assert_eq!(
        secrets.stored_password().as_deref(),
        Some(rotated),
        "the passwordless save wrote a stale password back over the rotation",
    );
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/smtp/test")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let presented = sender.presented();
    assert_eq!(presented.len(), 1);
    let crate::server::ops::mailer::MailCredentials::Smtp(creds) = &presented[0];
    assert_eq!(creds.password.expose(), rotated);
    assert_eq!(creds.from_name, "Acme Inc");
}

#[tokio::test]
async fn a_pre_split_password_survives_a_passwordless_save() {
    // Credentials written before the password moved to its own key keep it
    // inside the configuration blob. A passwordless save rewrites that blob, so
    // without the migration the secret would be dropped on the floor and the
    // company would silently stop being able to send.
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let secrets = Arc::new(RotatingSecrets::default());
    let sender = Arc::new(RecordingMailSender::new());
    let state = state_with_secrets(
        &home,
        ConnectionsRuntime::new().with_mail(sender.clone()),
        Some(secrets.clone()),
    )
    .await;

    // Seed the old layout directly: one blob, password inside, no password key.
    let legacy = "legacy-pw";
    crate::ports::SecretStore::set(
        secrets.as_ref(),
        &CompanyId::new("acme"),
        super::SMTP_KEY,
        SecretValue(
            serde_json::json!({
                "host": "smtp.acme.test",
                "port": 587,
                "security": "starttls",
                "username": "mailer",
                "password": legacy,
                "from_name": "Acme",
                "from_email": "ceo@acme.test",
            })
            .to_string(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(secrets.stored_password(), None);

    let app = router(state);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v1/company/smtp")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "host": "smtp.acme.test",
                        "port": 587,
                        "security": "starttls",
                        "username": "mailer",
                        "from_name": "Acme Inc",
                        "from_email": "ceo@acme.test",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Migrated to its own key, and still the password that reaches the wire.
    assert_eq!(secrets.stored_password().as_deref(), Some(legacy));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/smtp/test")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let presented = sender.presented();
    let crate::server::ops::mailer::MailCredentials::Smtp(creds) = &presented[0];
    assert_eq!(creds.password.expose(), legacy);
    assert_eq!(creds.from_name, "Acme Inc");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rotation_racing_the_legacy_migration_is_not_lost() {
    // The one path that still reads and then writes: migrating a pre-split
    // password out of the config blob. A rotation arriving while that is in
    // flight must not be overwritten by it, so `put_smtp` serializes per
    // company. Whichever order the two land in, the rotation is what survives —
    // it is the later intent in both interleavings the lock permits.
    //
    // Run repeatedly: a lock that is missing shows up as an intermittent loss,
    // so a single pass would be a weak witness.
    for attempt in 0..8 {
        let home_dir = home();
        let home = home_dir.path().to_path_buf();
        let secrets = Arc::new(RotatingSecrets::default());
        let state = state_with_secrets(
            &home,
            ConnectionsRuntime::new(),
            Some(secrets.clone() as Arc<dyn crate::ports::SecretStore>),
        )
        .await;

        // Seed the pre-split layout: password inside the blob, no password key.
        let legacy = "legacy-pw";
        let rotated = "rotated-pw";
        crate::ports::SecretStore::set(
            secrets.as_ref(),
            &CompanyId::new("acme"),
            super::SMTP_KEY,
            SecretValue(
                serde_json::json!({
                    "host": "smtp.acme.test",
                    "port": 587,
                    "security": "starttls",
                    "username": "mailer",
                    "password": legacy,
                    "from_name": "Acme",
                    "from_email": "ceo@acme.test",
                })
                .to_string(),
            ),
        )
        .await
        .unwrap();

        // Hold the migration's read-modify-write window open so the rotation
        // has somewhere to land.
        secrets.stall_config_reads(40);

        let app = router(state);
        let put = |body: serde_json::Value| {
            let app = app.clone();
            async move {
                app.oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri("/api/v1/company/smtp")
                        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
            }
        };

        // The passwordless save (which must migrate) against the rotation.
        let migrating = put(serde_json::json!({
            "host": "smtp.acme.test",
            "port": 587,
            "security": "starttls",
            "username": "mailer",
            "from_name": "Acme Inc",
            "from_email": "ceo@acme.test",
        }));
        let rotating = put(serde_json::json!({
            "host": "smtp.acme.test",
            "port": 587,
            "security": "starttls",
            "username": "mailer",
            "password": rotated,
            "from_name": "Acme",
            "from_email": "ceo@acme.test",
        }));
        let (a, b) = tokio::join!(migrating, rotating);
        assert_eq!(a.status(), StatusCode::OK);
        assert_eq!(b.status(), StatusCode::OK);

        assert_eq!(
            secrets.stored_password().as_deref(),
            Some(rotated),
            "attempt {attempt}: the legacy migration overwrote a concurrent rotation",
        );
    }
}

#[test]
fn debugging_the_stored_config_does_not_print_a_legacy_password() {
    // `StoredConfig` is the one place a password can still ride along inside
    // otherwise non-secret configuration, so a `Debug` that printed it would
    // put a live credential into any log line or panic message that formats
    // one. Since issue #1770 the redaction comes from the field's type rather
    // than from a hand-written impl on this struct, so the marker is
    // `SECRET_REDACTED` and a plain `#[derive(Debug)]` here is safe.
    let config: super::smtp::StoredConfig = serde_json::from_value(serde_json::json!({
        "host": "smtp.acme.test",
        "port": 587,
        "security": "starttls",
        "username": "mailer",
        "password": "must-not-appear",
        "from_name": "Acme",
        "from_email": "ceo@acme.test",
    }))
    .unwrap();
    let rendered = format!("{config:?}");
    assert!(!rendered.contains("must-not-appear"), "{rendered}");
    assert!(
        rendered.contains(crate::ports::types::SECRET_REDACTED),
        "{rendered}"
    );
    // Still useful for diagnosis.
    assert!(rendered.contains("smtp.acme.test"), "{rendered}");
}

#[tokio::test]
async fn an_all_whitespace_password_counts_as_omitted() {
    // The other edge of "trim only decides whether one was supplied". A body
    // carrying `"   "` is a save that names no password, so it keeps the stored
    // one rather than replacing a working credential with blanks. Pinned because
    // `api-write-plane.md` states it as the contract.
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let secrets = Arc::new(RotatingSecrets::default());
    let sender = Arc::new(RecordingMailSender::new());
    let state = state_with_secrets(
        &home,
        ConnectionsRuntime::new().with_mail(sender.clone()),
        Some(secrets.clone() as Arc<dyn crate::ports::SecretStore>),
    )
    .await;
    let app = router(state);

    let stored = "stored-pw";
    let save = |password: serde_json::Value, from_name: &str| {
        let app = app.clone();
        let body = serde_json::json!({
            "host": "smtp.acme.test",
            "port": 587,
            "security": "starttls",
            "username": "mailer",
            "password": password,
            "from_name": from_name,
            "from_email": "ceo@acme.test",
        });
        async move {
            app.oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/company/smtp")
                    .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };

    assert_eq!(
        save(serde_json::json!(stored), "Acme").await.status(),
        StatusCode::OK
    );
    // Whitespace only: treated as omitted, so the stored password stands.
    assert_eq!(
        save(serde_json::json!("   "), "Acme Inc").await.status(),
        StatusCode::OK
    );
    assert_eq!(secrets.stored_password().as_deref(), Some(stored));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/company/smtp/test")
                .header("cookie", crate::server::test_support::fixed_cookie("acme"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let presented = sender.presented();
    let crate::server::ops::mailer::MailCredentials::Smtp(creds) = &presented[0];
    assert_eq!(creds.password.expose(), stored);
    assert_eq!(creds.from_name, "Acme Inc");
}
