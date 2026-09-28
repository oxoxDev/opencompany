use super::{ConnectionStateDto, CredentialSource, connect_route};

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use crate::company::CompanyManifest;
use crate::ports::types::{CompanyId, CompanyRecord, SecretValue};
use crate::runtime::RuntimeBuilder;
use crate::server::router;
use crate::store::FsCompanyStore;
use crate::{AppConfig, AppState};

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("oc-connections-")
        .tempdir()
        .expect("tempdir")
}

async fn state_with_manifest(home: &std::path::Path, manifest_toml: &str) -> AppState {
    use crate::ports::CompanyStore;
    let manifest: CompanyManifest = toml::from_str(manifest_toml).unwrap();
    let store = FsCompanyStore::new(home.to_path_buf());
    let id = CompanyId::new("acme");
    store
        .save(&CompanyRecord {
            general_channel: Default::default(),
            overlay_desk_hive: Vec::new(),
            overlay_retired_agents: Vec::new(),
            overlay_agent_edits: Vec::new(),
            id: id.clone(),
            manifest: manifest.clone(),
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
    let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest)
        .with_id(id.clone())
        .build()
        .await
        .unwrap();
    let state = AppState::new(AppConfig::default());
    state.registry().insert(id, std::sync::Arc::new(runtime));
    crate::server::test_support::seed_fixed_admin(&state, "acme").await;
    state
}

async fn get_connections(state: &AppState) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/company/connections")
        .header("cookie", crate::server::test_support::fixed_cookie("acme"))
        .body(Body::empty())
        .unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// The list route projects a connected provider (token stored) and a
/// not-connected one (no token) side by side — the shape the console needs
/// to flip from "unavailable" to live buttons.
#[tokio::test]
async fn projects_connected_and_not_connected() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(
        &home,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [[connection]]\nprovider = \"github\"\n\
         [[connection]]\nprovider = \"slack\"\n\
         [[connection]]\nprovider = \"gmail\"\n",
    )
    .await;

    // Store a GitHub token blob (github connected, with an account label);
    // a Gmail token blob with NO account label (connected, account omitted);
    // slack stays untouched (not connected).
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();
    runtime
        .secrets()
        .set(
            &id,
            "oauth/github",
            SecretValue(
                serde_json::json!({
                    "token": { "access_token": "gho_secret_should_never_leak" },
                    "account": "octocat"
                })
                .to_string(),
            ),
        )
        .await
        .unwrap();
    runtime
        .secrets()
        .set(
            &id,
            "oauth/gmail",
            SecretValue(
                serde_json::json!({
                    "token": { "access_token": "ya29_secret_should_never_leak" }
                })
                .to_string(),
            ),
        )
        .await
        .unwrap();

    let (status, body) = get_connections(&state).await;
    assert_eq!(status, StatusCode::OK);
    let list = body.as_array().expect("array body");
    assert_eq!(list.len(), 3, "one row per manifest connection: {body}");

    let github = list
        .iter()
        .find(|c| c["provider"] == "github")
        .expect("github row");
    assert_eq!(github["connected"], true);
    assert_eq!(github["account"], "octocat");
    // A stored token is the BYO override and outranks every host tier, so
    // this is `static` whatever the ambient environment happens to hold.
    assert_eq!(github["credentialSource"], "static", "{github}");

    let slack = list
        .iter()
        .find(|c| c["provider"] == "slack")
        .expect("slack row");
    assert_eq!(slack["connected"], false);
    assert!(
        slack.get("account").is_none(),
        "no account when not connected: {slack}"
    );
    // Nothing stored, so this row reports whichever host tier the ambient
    // environment resolves to. Assert only that the field is present and a
    // known tier name — the exhaustive precedence matrix is pinned against a
    // `MapEnv` in `connect_route_*` below, where it does not depend on the
    // test runner's environment.
    let tier = slack["credentialSource"]
        .as_str()
        .expect("every row carries a credentialSource");
    assert!(
        matches!(tier, "attested" | "static" | "none"),
        "unknown credential tier {tier:?}"
    );

    // A connected provider whose stored blob carries no `account` label is
    // still `connected: true`, and the `account` field is omitted entirely
    // (never serialized as null) — the `skip_serializing_if` path.
    let gmail = list
        .iter()
        .find(|c| c["provider"] == "gmail")
        .expect("gmail row");
    assert_eq!(gmail["connected"], true);
    assert!(
        gmail.get("account").is_none(),
        "no account when the stored blob carries no label: {gmail}"
    );

    // SECURITY: no token material may appear anywhere in the response.
    assert!(
        !body.to_string().contains("gho_secret_should_never_leak"),
        "token material leaked into the connections response: {body}"
    );
    assert!(
        !body.to_string().contains("ya29_secret_should_never_leak"),
        "token material leaked into the connections response: {body}"
    );
    assert!(
        !body.to_string().contains("access_token"),
        "token field leaked into the connections response: {body}"
    );
    // The new field is a tier name, so it must not have opened a path for a
    // token file, an api key, or a client secret to ride along.
    for forbidden in [
        crate::company::credentials::TOKEN_FILE_ENV,
        crate::company::credentials::API_KEY_ENV,
        "OPENCOMPANY_OAUTH_",
        "clientSecret",
        "client_secret",
    ] {
        assert!(
            !body.to_string().contains(forbidden),
            "{forbidden} leaked into the connections response: {body}"
        );
    }
}

/// The precedence matrix from [`connect_route`], driven through the env seam
/// so nothing mutates the process environment: stored wins, then a projected
/// platform identity, then nothing. A configured native provider app is no
/// longer a route after #838.
#[test]
fn connect_route_never_advertises_a_retired_native_provider_app() {
    use crate::app::config::MapEnv;

    let dir = tempfile::Builder::new()
        .prefix("oc-connect-route-")
        .tempdir()
        .expect("tempdir");
    let path = dir.path().join("token");
    std::fs::write(&path, "projected-instance-token").unwrap();
    let projected = MapEnv::new([(
        crate::company::credentials::TOKEN_FILE_ENV,
        path.display().to_string(),
    )]);
    let retired_native_app = MapEnv::new([
        ("OPENCOMPANY_OAUTH_GITHUB_ID", "fake-client-id"),
        ("OPENCOMPANY_OAUTH_GITHUB_SECRET", "fake-client-secret"),
    ]);

    // 1. A historical stored token is still visible and revocable. It
    //    outranks every host-level source for that description.
    assert_eq!(
        connect_route("github", true, &projected),
        CredentialSource::Static
    );
    assert_eq!(
        connect_route("github", true, &MapEnv::default()),
        CredentialSource::Static
    );

    // 2. Nothing stored + a projected platform identity → the platform
    //    owns the connection, for every provider (the identity is host-level).
    assert_eq!(
        connect_route("github", false, &projected),
        CredentialSource::Attested
    );
    assert_eq!(
        connect_route("slack", false, &projected),
        CredentialSource::Attested
    );

    assert_eq!(
        connect_route("github", false, &retired_native_app),
        CredentialSource::None,
        "a configured native app must not be offered after its start route retires"
    );

    // 3. Neither → no connection route exists on this host.
    assert_eq!(
        connect_route("github", false, &MapEnv::default()),
        CredentialSource::None
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The regression this restriction exists for: a self-hoster who set
/// `TINYHUMANS_API_KEY` to buy inference has **no** platform-run connection
/// route, so their Connect must not be reported as platform-managed and
/// taken away from them.
///
/// `TinyhumansTokenSource::from_env` happily resolves that key as its static
/// tier; [`connect_route`] deliberately accepts only the projected-file tier.
#[test]
fn a_static_api_key_is_not_a_hosted_connection_route() {
    use crate::app::config::MapEnv;

    // Inference credential only, nothing else: NOT attested.
    let inference_only = MapEnv::new([(crate::company::credentials::API_KEY_ENV, "th_fake_key")]);
    assert_eq!(
        connect_route("github", false, &inference_only),
        CredentialSource::None,
        "a static inference key must not be read as a platform connection route"
    );

    // And a `TINYHUMANS_TOKEN_FILE` naming a path that does not exist
    // degrades to the static tier inside the resolver — which is likewise
    // not a hosted connection route.
    let dangling = MapEnv::new([
        (
            crate::company::credentials::TOKEN_FILE_ENV,
            "/nonexistent/oc/projected/token",
        ),
        (crate::company::credentials::API_KEY_ENV, "th_fake_key"),
    ]);
    assert_eq!(
        connect_route("github", false, &dangling),
        CredentialSource::None,
        "a token file that was never projected is not a hosted connection route"
    );
}

/// The DTO's whole serialized surface: three non-secret facts plus an
/// optional label. Pins the field set so a future addition has to be a
/// deliberate edit here, and pins the camelCase spelling the console reads.
#[test]
fn the_dto_carries_a_tier_name_and_nothing_secret() {
    let dto = ConnectionStateDto {
        provider: "github".to_string(),
        connected: true,
        credential_source: CredentialSource::Attested,
        account: Some("octocat".to_string()),
        via: vec!["native"],
        unverified: false,
    };
    let json = serde_json::to_value(&dto).unwrap();
    assert_eq!(json["credentialSource"], "attested");
    let mut keys: Vec<&String> = json.as_object().unwrap().keys().collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "account",
            "connected",
            "credentialSource",
            "provider",
            "unverified",
            "via"
        ],
        "the read shape must stay exactly this: {keys:?}"
    );
}

/// Issue #316: one coherent answer per provider.
///
/// Without the `composio` feature there is no second namespace, so every
/// row's answer is the native one — and, critically, `unverified` is
/// **false**: "there is no Composio path here" must not be reported as "we
/// could not check". The `composio` build's live probe is exercised against
/// the real backend, not here; what this pins is the reconciliation shape
/// both read planes now serve.
#[tokio::test]
async fn a_provider_in_neither_namespace_reads_disconnected_not_unknown() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(
        &home,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [[connection]]\nprovider = \"github\"\n\
         [[connection]]\nprovider = \"slack\"\n",
    )
    .await;

    // github connected natively; slack in neither namespace.
    let id = CompanyId::new("acme");
    let runtime = state.registry().get(&id).unwrap();
    runtime
        .secrets()
        .set(
            &id,
            "oauth/github",
            SecretValue(
                serde_json::json!({
                    "token": { "access_token": "gho_fake_never_a_real_token" },
                    "account": "octocat"
                })
                .to_string(),
            ),
        )
        .await
        .unwrap();

    let (status, body) = get_connections(&state).await;
    assert_eq!(status, StatusCode::OK);
    let list = body.as_array().expect("array body");

    // Exactly one row per provider — never one per namespace.
    assert_eq!(list.len(), 2, "one row per provider: {body}");

    let github = list
        .iter()
        .find(|row| row["provider"] == "github")
        .expect("a github row");
    assert_eq!(github["connected"], true);
    assert_eq!(github["via"], serde_json::json!(["native"]));
    assert_eq!(github["unverified"], false);

    let slack = list
        .iter()
        .find(|row| row["provider"] == "slack")
        .expect("a slack row");
    assert_eq!(slack["connected"], false);
    assert_eq!(
        slack["via"],
        serde_json::json!([]),
        "no namespace claims it"
    );
    assert_eq!(
        slack["unverified"], false,
        "no Composio path is a known answer, not an unknown one"
    );
}

/// The provider-id → Composio-slug normalization that lets one provider
/// resolve to one row across both namespaces. The console spells ids
/// hyphenated (`google-calendar`); Composio spells slugs unpunctuated
/// (`googlecalendar`). Without a shared rule these are two providers, and the
/// page reports both — which is the #316 symptom.
#[test]
fn provider_ids_and_composio_slugs_normalize_to_one_key() {
    use super::toolkit_slug;
    assert_eq!(toolkit_slug("google-calendar"), "googlecalendar");
    assert_eq!(toolkit_slug("google-drive"), "googledrive");
    assert_eq!(toolkit_slug("GitHub"), "github");
    assert_eq!(toolkit_slug("gmail"), "gmail");
}

/// Every `toolkit` the console authorizes with must already be in this
/// normalizer's canonical form (issue #599).
///
/// The console states its eleven Composio slugs explicitly rather than
/// deriving them — `x` maps to `twitter`, which no normalization rule
/// produces — and its doc calls the table a mirror of [`toolkit_slug`]. A
/// mirror with no reflection test drifts silently: loosen or tighten the
/// rule here and those eleven tiles keep authorizing against slugs this
/// host no longer reconciles rows under, surfacing as "provider not
/// enabled" — the exact symptom #599 fixed.
///
/// So this reads the real console catalog and feeds it through the real
/// normalizer. It asserts a fixed point (`toolkit_slug(t) == t`) rather
/// than `toolkit_slug(id) == toolkit`, because the latter is false for
/// `x`/`twitter` by design — the alias is the reason the table is explicit.
#[test]
fn console_toolkit_slugs_are_canonical_under_this_normalizer() {
    use super::toolkit_slug;

    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../frontend/src/lib/connections.ts"
    );
    let source = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read the console connection catalog at {path}: {err}"));

    let slugs: Vec<String> = source
        .lines()
        .filter_map(|line| line.trim().strip_prefix("toolkit: \""))
        .filter_map(|rest| rest.split('"').next())
        .map(str::to_string)
        .collect();

    // Guard against a silent pass: a renamed field or a reformatted catalog
    // would otherwise leave this asserting over an empty list.
    assert_eq!(
        slugs.len(),
        11,
        "expected one `toolkit:` per console tile, found {}: {slugs:?}. If the \
         catalog legitimately changed size, update this count; if the field was \
         renamed, update the parse.",
        slugs.len()
    );

    for slug in &slugs {
        assert_eq!(
            &toolkit_slug(slug),
            slug,
            "console toolkit {slug:?} is not canonical under toolkit_slug; the \
             console would authorize a slug this host reconciles rows under a \
             different key"
        );
    }
}

/// Issue #582: the Composio half of a row does **not** depend on the
/// `composio` tool grant.
///
/// This is the contradiction the Connections page reported. `GET
/// …/connections` used to discard every Composio connection unless the
/// company explicitly granted the `composio` namespace, while `GET
/// …/composio/connections` — which the console's provider list reads —
/// never consulted the grant. 13 of the 21 shipped companies grant no
/// `composio`, so for most of them "connected here, not connected there"
/// was the steady state rather than a race.
///
/// The manifest below grants nothing at all, which is the case that used to
/// return an empty list: no `[[connection]]` to project natively, and a
/// Composio view thrown away before it was read.
#[tokio::test]
async fn composio_state_survives_a_company_that_does_not_grant_composio() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(
        &home,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n",
    )
    .await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();

    let rows = super::reconcile(
        runtime.as_ref(),
        super::ComposioView::Known(
            [("gmail".to_string(), true), ("slack".to_string(), false)]
                .into_iter()
                .collect(),
        ),
    )
    .await
    .expect("reconcile");

    let gmail = rows
        .iter()
        .find(|row| row.provider == "gmail")
        .expect("a Composio-connected provider must reach the console even with no grant");
    assert!(
        gmail.connected,
        "gmail should be connected: {:?}",
        gmail.via
    );
    assert_eq!(gmail.via, vec!["composio"]);
    // A toolkit Composio knows but has not connected is not a row: the page
    // lists what is connected plus what the catalog offers, and the catalog
    // is the other route's job.
    assert!(
        !rows.iter().any(|row| row.provider == "slack"),
        "a disconnected toolkit should not become a connection row"
    );
}

/// An unread probe is reported as unknown, not as disconnected — the
/// distinction the console renders as "could not check".
#[tokio::test]
async fn an_unreadable_composio_probe_is_unverified_not_disconnected() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(
        &home,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n\
         [[connection]]\nprovider = \"gmail\"\n",
    )
    .await;
    let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();

    let rows = super::reconcile(runtime.as_ref(), super::ComposioView::Unavailable)
        .await
        .expect("reconcile");

    let gmail = rows.iter().find(|row| row.provider == "gmail").unwrap();
    assert!(!gmail.connected);
    assert!(
        gmail.unverified,
        "an unanswered probe must not read as 'no'"
    );
}

/// A company with no `[[connection]]` entries returns an empty list (200),
/// not a 404 — so the console renders "ready" with an empty catalog rather
/// than the "unavailable" fallback.
#[tokio::test]
async fn empty_when_no_connections_declared() {
    let home_dir = home();
    let home = home_dir.path().to_path_buf();
    let state = state_with_manifest(
        &home,
        "[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n",
    )
    .await;

    let (status, body) = get_connections(&state).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]), "empty array: {body}");
}
