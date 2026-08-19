//! The repository binding write plane (issue #245, operator half): bind a real
//! repository to a company, list what is bound, revoke one.
//!
//! Three routes and no more. There is deliberately **no agent surface** behind
//! them in this change — no grant, no tool, nothing that puts a checkout in a
//! workspace during a turn. What ships here is the credential path, whole: an
//! operator installs a token, the host uses it to mirror the repository, and
//! every read of this surface is redacted by construction.
//!
//! The credential rules this surface enforces, and why each one is a route
//! concern rather than a documentation concern:
//!
//! * **A classic PAT is refused.** `ghp_…` can read every repository the
//!   account can reach, which is the blast radius the whole design is arranged
//!   to avoid. Refusing it at intake — with the steps to make a fine-grained
//!   one — is the only point at which an operator is still holding the token
//!   and can go make a better one.
//! * **A failed bind stores nothing.** The token is written before the network
//!   is touched (it is what touches it), so a bad credential or an unreachable
//!   repository must roll the whole thing back. See
//!   [`RepoManager::bind`](crate::runtime::RepoManager::bind).
//! * **No read echoes a token.** The list shape is the persisted binding, which
//!   carries a fingerprint and never the credential.
//!
//! Both scope forms (`…/companies/{id}` and the single-company alias
//! `…/company`) are registered by [`scoped`], and the two mutations require
//! authority over the company (issue #403): what a company's agents will read,
//! and under whose credential, is decided *for* the company.

use axum::extract::Path;
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use serde::Serialize;

use crate::AppState;
use crate::company::runtime::CompanyRuntime;
use crate::runtime::RepoManager;
use crate::runtime::repo_manager::types::{BindRequest, RepoBinding};
use crate::server::error::ApiError;
use crate::server::ops::mcp::RosterAgentDto;
use crate::server::ops::{AdminScopedCompany, ScopedCompany, scoped};

/// Builds the repository binding route fragment.
pub fn router() -> Router<AppState> {
    scoped("/repos", post(bind_repo).get(list_repos))
        .merge(scoped("/repos/{key}/revoke", post(revoke_repo)))
}

/// The list body.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepoList {
    /// The bound repositories. Each carries a credential *fingerprint*, never
    /// a credential — see [`RepoBinding`].
    repos: Vec<RepoBinding>,
    /// Whether this host can also read pull requests (metadata + unified diff),
    /// which needs a forge HTTP client the default build does not link. The
    /// console uses it to say so rather than offering a control that 501s.
    pull_requests_available: bool,
    /// The roster agents whose **effective** tool grants include `repo` (issue
    /// #245, agent half) — i.e. who can actually check any of this out.
    ///
    /// One list for the whole company rather than one per binding, because the
    /// grant is namespace-wide: `repo` confers every binding or none, and a
    /// per-binding copy of one list would imply a per-repository control that
    /// does not exist. Empty is the misconfiguration the card names: a company
    /// that bound repositories nobody is allowed to read.
    ///
    /// Each entry carries the label to print as well as the id (issue #931) —
    /// see [`RosterAgentDto`].
    granted_agents: Vec<RosterAgentDto>,
}

/// The reminder attached to every mutating response.
///
/// Mirrors the MCP surface's note, and says the same true thing for the same
/// reason: nothing about this is wired at boot, so a change is live as soon as
/// it is saved.
const LIVE_NOTE: &str = "Saved. The mirror is on this host now — no restart needed.";

/// A mutating response: the resulting binding and the reminder.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MutationResponse {
    /// The binding, redacted as always.
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<RepoBinding>,
    /// A short human-readable confirmation.
    note: String,
}

/// Resolves the company's repository manager.
///
/// `None` is a real deployment state, not a bug: the manager is constructed
/// from a filesystem home, so a runtime assembled without one (a test harness,
/// an embedding that supplies its own ports) simply has no repository cache.
/// Each handler turns that into [`not_wired`](super::not_wired), whose 404 the
/// console's bare-catch reads as "this host does not do that" — so the card
/// hides itself rather than showing an error, which is the right outcome.
///
/// `Option`, not `Result<_, Response>`: an `axum` `Response` is 128+ bytes, and
/// `clippy::result_large_err` is right that a helper whose `Ok` is a reference
/// should not make every successful call carry the refusal's footprint. Same
/// call the sibling refusal helpers in `team.rs` and `team_agent.rs` make.
fn manager(runtime: &CompanyRuntime) -> Option<&RepoManager> {
    runtime.repos().map(|m| m.as_ref())
}

/// The 404 every handler answers when no cache is wired.
fn no_repo_surface() -> Response {
    super::not_wired("repository binding")
}

// -- POST repos --------------------------------------------------------------

/// `POST …/repos` — bind a repository.
///
/// Requires authority over the company (issue #403): this installs a credential
/// the company's host-side fetches will run under.
async fn bind_repo(
    company: AdminScopedCompany,
    Json(body): Json<BindRequest>,
) -> Result<Json<MutationResponse>, Response> {
    use axum::response::IntoResponse;
    let Some(repos) = manager(&company.runtime) else {
        return Err(no_repo_surface());
    };
    let binding = repos
        .bind(body)
        .await
        .map_err(|e| ApiError(e).into_response())?;
    tracing::info!(
        company = %company.id(),
        key = %binding.key,
        url = %binding.url,
        // The fingerprint, never the token — this line is the audit trail for
        // "who installed which credential", and it has to be safe to keep.
        credential = %binding.token_fingerprint,
        "bound a repository"
    );
    Ok(Json(MutationResponse {
        repo: Some(binding),
        note: LIVE_NOTE.to_string(),
    }))
}

// -- GET repos ---------------------------------------------------------------

/// `GET …/repos` — what is bound.
///
/// [`ScopedCompany`] rather than [`AdminScopedCompany`], matching the MCP
/// server list: which repositories a company reads is part of knowing what the
/// company is, and the response carries no credential material for a member to
/// learn anything from.
async fn list_repos(company: ScopedCompany) -> Result<Json<RepoList>, Response> {
    use axum::response::IntoResponse;
    let Some(repos) = manager(&company.runtime) else {
        return Err(no_repo_surface());
    };
    let list = repos
        .list()
        .await
        .map_err(|e| ApiError(e).into_response())?;
    Ok(Json(RepoList {
        repos: list,
        pull_requests_available: repos.has_host(),
        granted_agents: granted_agents(&company.runtime).await,
    }))
}

/// The roster agents whose effective grants include `repo`.
///
/// Read through the **same** [`roster_grants`] the MCP surface uses, which is
/// itself the roster the harness builds — so what this page says an agent can
/// reach is what `build_agent` actually wires, rather than a second answer
/// derived from the manifest by hand.
///
/// Each agent carries the label the card prints alongside its id (issue #931).
/// The id alone is readable only for a manifest agent; an operator-added
/// teammate's is minted, so "Readable by" named half a company's teammates in
/// internal strings — the same defect the MCP surface's "Reachable by" line had,
/// which sharing one roster walk with it makes a single fix.
///
/// A store error degrades to an empty list rather than failing the read: the
/// bindings are what this route is for, and losing the coverage annotation is a
/// smaller loss than losing the page. Empty then reads as "nobody", which is the
/// safe direction — it prompts an operator to check rather than reassuring them.
async fn granted_agents(runtime: &CompanyRuntime) -> Vec<RosterAgentDto> {
    let Ok(Some(record)) = runtime.store().load(runtime.id()).await else {
        return Vec::new();
    };
    super::mcp::roster_grants(&record)
        .into_iter()
        .filter(|(_, grants)| crate::company::grants_repo_explicit(grants))
        .map(|(agent, _)| agent)
        .collect()
}

// -- POST repos/{key}/revoke -------------------------------------------------

/// `POST …/repos/{key}/revoke` — unbind a repository.
///
/// A `POST …/revoke` rather than `DELETE …/repos/{key}`, because it is not only
/// a deletion: it blanks the stored credential and removes the mirror from
/// disk, and it is the *only* way cache space comes back (the mirrors never
/// prune — see [`RepoManager`]). Naming the action keeps that visible.
///
/// Requires authority over the company — the inverse of the bind, and no less
/// consequential: it takes the company's agents' access to that code away.
async fn revoke_repo(
    company: AdminScopedCompany,
    Path(params): Path<RevokePath>,
) -> Result<Json<MutationResponse>, Response> {
    use axum::response::IntoResponse;
    let Some(repos) = manager(&company.runtime) else {
        return Err(no_repo_surface());
    };
    repos
        .revoke(&params.key)
        .await
        .map_err(|e| ApiError(e).into_response())?;
    tracing::info!(company = %company.id(), key = %params.key, "revoked a repository binding");
    Ok(Json(MutationResponse {
        repo: None,
        note: format!(
            "Unbound {}. Its mirror and credential are gone.",
            params.key
        ),
    }))
}

/// The revoke path parameters.
///
/// `id` is captured too because the platform scope form carries it; the alias
/// form does not, so it is optional. Extracting the pair as a struct is what
/// lets one handler serve both registrations.
#[derive(Debug, serde::Deserialize)]
struct RevokePath {
    /// The binding key.
    key: String,
    /// The company id, on the platform scope form only.
    #[allow(dead_code, reason = "consumed by the ScopedCompany extractor")]
    id: Option<String>,
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::company::CompanyManifest;
    use crate::ports::CompanyStore;
    use crate::ports::types::{CompanyId, CompanyRecord};
    use crate::runtime::RuntimeBuilder;
    use crate::runtime::repo_manager::repo_token_key;
    use crate::server::router;
    use crate::{AppConfig, AppState};

    /// The credential every refusal test looks for afterwards.
    const SENTINEL: &str = "github_pat_SENTINEL";

    fn manifest() -> CompanyManifest {
        toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap()
    }

    /// One running company `acme` over a temp home, with a fixed admin session.
    async fn state_with(home: &std::path::Path) -> AppState {
        let store = crate::store::FsCompanyStore::new(home.to_path_buf());
        let id = CompanyId::new("acme");
        store
            .save(&CompanyRecord {
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
                disabled_workflows: Vec::new(),
                template_provenance: None,
                setup: None,
                overlay_policy: None,
                overlay_desk_tools: Default::default(),
            })
            .await
            .unwrap();
        let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest())
            .with_id(id.clone())
            .build()
            .await
            .unwrap();
        let state = AppState::new(AppConfig::default()).with_home(home.to_path_buf());
        state.registry().insert(id, Arc::new(runtime));
        crate::server::test_support::seed_fixed_admin(&state, "acme").await;
        state
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("cookie", crate::server::test_support::fixed_cookie("acme"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(uri)
            .header("cookie", crate::server::test_support::fixed_cookie("acme"))
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn a_classic_pat_is_refused_at_the_route_and_nothing_is_stored() {
        let home = tempfile::Builder::new()
            .prefix("oc-repos-route-")
            .tempdir()
            .unwrap();
        let state = state_with(home.path()).await;
        let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();

        let response = router(state.clone())
            .oneshot(post(
                "/api/v1/company/repos",
                serde_json::json!({
                    "url": "https://github.com/acme/widgets",
                    "token": "ghp_classicclassicclassic",
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        let message = body["error"].as_str().unwrap_or_default().to_string();
        assert!(message.contains("fine-grained"), "{message}");

        // The credential the operator pasted must not be sitting in the store
        // after a refusal — that is the whole reason the check is at intake.
        // The key is derived, not written out: a hard-coded one that no longer
        // matches the derivation would make this assertion pass vacuously.
        let key =
            crate::runtime::repo_manager::types::parse_repo_url("https://github.com/acme/widgets")
                .unwrap()
                .key();
        let stored = runtime
            .secrets()
            .get(runtime.id(), &repo_token_key(&key))
            .await
            .unwrap();
        assert!(stored.is_none(), "a refused credential was stored");
    }

    /// Issue #752: the operator-facing half of the plaintext-secret gate. This
    /// host is fs-backed (the [`AppState`] default, which is what a local or
    /// self-hosted `serve` actually is), so the route refuses the bind and says
    /// what to do about it — rather than accepting a credential that would sit
    /// in a file the agent shell can read.
    #[tokio::test]
    async fn binding_is_refused_on_a_host_that_keeps_secrets_on_its_own_disk() {
        let home = tempfile::Builder::new()
            .prefix("oc-repos-plaintext-")
            .tempdir()
            .unwrap();
        let state = state_with(home.path()).await;
        let runtime = state.registry().get(&CompanyId::new("acme")).unwrap();

        let response = router(state.clone())
            .oneshot(post(
                "/api/v1/company/repos",
                serde_json::json!({
                    "url": "https://github.com/acme/widgets",
                    "token": SENTINEL,
                }),
            ))
            .await
            .unwrap();
        // 409 rather than 400: the request is fine, the deployment is not, and
        // the operator fixes it by changing the host or the grant and retrying.
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = body_json(response).await;
        let message = body["error"].as_str().unwrap_or_default().to_string();
        assert!(message.contains("OPENCOMPANY_STORAGE=fs"), "{message}");
        assert!(message.contains("OPENCOMPANY_STORAGE=mongodb"), "{message}");
        assert!(message.contains("`repo` grant"), "{message}");
        // The refusal must not echo the credential into a response body.
        assert!(!message.contains(SENTINEL), "{message}");

        let key =
            crate::runtime::repo_manager::types::parse_repo_url("https://github.com/acme/widgets")
                .unwrap()
                .key();
        let stored = runtime
            .secrets()
            .get(runtime.id(), &repo_token_key(&key))
            .await
            .unwrap();
        assert!(stored.is_none(), "a refused credential was stored");
    }

    #[tokio::test]
    async fn a_non_github_url_is_refused_at_the_route() {
        let home = tempfile::Builder::new()
            .prefix("oc-repos-url-")
            .tempdir()
            .unwrap();
        let state = state_with(home.path()).await;
        let response = router(state)
            .oneshot(post(
                "/api/v1/company/repos",
                serde_json::json!({
                    "url": "https://gitlab.com/acme/widgets",
                    "token": SENTINEL,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_list_route_answers_empty_and_reports_the_pull_request_capability() {
        let home = tempfile::Builder::new()
            .prefix("oc-repos-list-")
            .tempdir()
            .unwrap();
        let state = state_with(home.path()).await;
        let response = router(state)
            .oneshot(get("/api/v1/company/repos"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["repos"].as_array().unwrap().len(), 0);
        // A company that grants no `repo` namespace has nobody who could read a
        // binding, which is what the card's "bound but nobody holds `repo`"
        // notice is written from.
        assert_eq!(body["grantedAgents"].as_array().unwrap().len(), 0);
        // Wired exactly when the forge HTTP client is compiled in, so the
        // console can say "this host can't read diffs" instead of offering a
        // control that answers 501.
        assert_eq!(
            body["pullRequestsAvailable"].as_bool(),
            Some(cfg!(feature = "github"))
        );
    }

    #[tokio::test]
    async fn revoking_something_unbound_is_a_not_found() {
        let home = tempfile::Builder::new()
            .prefix("oc-repos-revoke-")
            .tempdir()
            .unwrap();
        let state = state_with(home.path()).await;
        let response = router(state)
            .oneshot(post(
                "/api/v1/company/repos/nope/revoke",
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn both_scope_forms_are_registered() {
        let home = tempfile::Builder::new()
            .prefix("oc-repos-scope-")
            .tempdir()
            .unwrap();
        let state = state_with(home.path()).await;
        // The platform `{id}` form must resolve the same surface as the
        // single-company alias, or a hosted tenant has no repositories page.
        let response = router(state)
            .oneshot(get("/api/v1/companies/acme/repos"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn the_list_body_carries_no_credential() {
        let list = RepoList {
            repos: vec![RepoBinding {
                key: "acme-widgets".into(),
                url: "https://github.com/acme/widgets".into(),
                owner: "acme".into(),
                repo: "widgets".into(),
                branches: vec!["main".into()],
                token_fingerprint: crate::runtime::repo_manager::types::fingerprint(
                    "github_pat_SENTINEL",
                ),
                last_fetched_millis: Some(1),
                size_bytes: 42,
                bound_at_millis: 1,
                can_push: None,
            }],
            pull_requests_available: false,
            granted_agents: vec![RosterAgentDto {
                id: "ceo".to_string(),
                name: "Chief".to_string(),
            }],
        };
        let json = serde_json::to_string(&list).unwrap();
        assert!(!json.contains("SENTINEL"), "{json}");
        assert!(json.contains("tokenFingerprint"), "{json}");
        assert!(json.contains("pullRequestsAvailable"), "{json}");
        assert!(json.contains("grantedAgents"), "{json}");
        // Issue #931: the card has a label to print, not only an id — for an
        // operator-added teammate the id is a minted internal string.
        assert!(json.contains(r#""name":"Chief""#), "{json}");
    }

    #[test]
    fn a_mutation_response_omits_the_repo_on_revoke() {
        let json = serde_json::to_string(&MutationResponse {
            repo: None,
            note: "gone".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"note":"gone"}"#);
    }
}
