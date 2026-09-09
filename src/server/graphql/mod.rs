//! GraphQL read plane: the single read surface behind every console view.
//!
//! The schema is rooted at a [`Company`](company::CompanyGql) aggregation
//! object so a view fetches everything it needs in one round trip; the only
//! top-level queries are `companies`, `company(id)`, and `skillRegistry`. The
//! [`Schema`] is built **once at startup** ([`build_schema`]) and stored on
//! [`AppState`](crate::AppState); each request injects its resolved
//! [`GqlAuth`](auth::GqlAuth) principal via request data. Mutations and
//! subscriptions are out of scope — REST owns the write plane.

pub mod auth;
pub mod company;
pub mod connections;
pub mod finances;
pub mod inbox;
pub mod memory_facts;
pub mod observability;
mod pagination;
mod policy;
pub mod skills;
pub mod tasks;
pub mod usage;
pub mod workflows;
pub mod workspace;

use async_graphql::{Context, EmptyMutation, EmptySubscription, ID, Object, Schema};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Router, extract::State};

use crate::AppState;
use crate::ports::types::CompanyId;
use auth::{GqlAuth, resolve_principal};
use company::CompanyGql;
use skills::RegistrySkillGql;

/// The concrete schema type stored on [`AppState`].
pub type OcSchema = Schema<QueryRoot, EmptyMutation, EmptySubscription>;

/// Builds the read-plane schema once. It carries no request data; per-request
/// [`AppState`] and [`GqlAuth`] are injected by [`graphql_handler`].
pub fn build_schema() -> OcSchema {
    Schema::build(QueryRoot, EmptyMutation, EmptySubscription).finish()
}

/// The schema's SDL, for snapshot tests and query-authoring against the contract.
pub fn sdl() -> String {
    build_schema().sdl()
}

/// Builds the GraphQL route fragment, merged into the main router.
///
/// `POST /graphql` serves queries; `GET /graphql` serves an embedded GraphiQL
/// explorer for interactive use during development.
///
/// The same handler is also mounted through [`scoped`](crate::server::ops::scoped),
/// giving `POST /api/v1/companies/{id}/graphql` alongside the
/// `/api/v1/company/graphql` alias — the identical pair every REST route gets.
/// A console addressing one of several companies on an origin names it in the
/// path, exactly as it already does for REST, so the request says which company
/// it means instead of the host inferring it.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/graphql", post(graphql_handler))
        .route("/graphql", get(graphiql))
        .merge(crate::server::ops::scoped(
            "/graphql",
            post(graphql_handler),
        ))
}

/// The query root: the three top-level entry points into the read plane.
pub struct QueryRoot;

#[Object(name = "Query")]
impl QueryRoot {
    /// Every company visible to the caller: all registered companies for the
    /// operator / platform-scope principal, or just a tenant's own in platform
    /// mode.
    async fn companies(&self, ctx: &Context<'_>) -> async_graphql::Result<Vec<CompanyGql>> {
        let state = ctx.data::<AppState>()?;
        let auth = ctx.data::<GqlAuth>()?;
        let mut out = Vec::new();
        for id in auth.visible_companies(state) {
            if let Some(runtime) = state.registry().get(&id) {
                out.push(CompanyGql::new(id, runtime));
            }
        }
        Ok(out)
    }

    /// One company by id, or — when `id` is omitted in single-company mode — the
    /// sole registered company. `null` when no such company is registered.
    async fn company(
        &self,
        ctx: &Context<'_>,
        id: Option<ID>,
    ) -> async_graphql::Result<Option<CompanyGql>> {
        let state = ctx.data::<AppState>()?;
        let auth = ctx.data::<GqlAuth>()?;
        let runtime = match &id {
            Some(id) => state.registry().get(&CompanyId::new(id.as_str())),
            None => state.registry().sole(),
        };
        let Some(runtime) = runtime else {
            return Ok(None);
        };
        let company = runtime.id().clone();
        auth.authorize(state, &company)?;
        Ok(Some(CompanyGql::new(company, runtime)))
    }

    /// The repo-level shared skill registry (`skills/*/SKILL.md`), installable
    /// into any company. Unscoped — the library is the same for every caller.
    async fn skill_registry(
        &self,
        ctx: &Context<'_>,
    ) -> async_graphql::Result<Vec<RegistrySkillGql>> {
        skills::resolve_registry(ctx).await
    }
}

/// `POST /graphql` — executes a query against the prebuilt schema.
///
/// The schema is built once and lives on [`AppState`]; each request injects a
/// cheap `AppState` clone and the resolved [`GqlAuth`] principal as request
/// data. An unauthenticated request in a guarded mode returns a single
/// `unauthorized` error instead of executing.
async fn graphql_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth::MaybePeer(peer): auth::MaybePeer,
    company: Option<axum::extract::Path<String>>,
    req: GraphQLRequest,
) -> GraphQLResponse {
    // Present on the `{id}` form, absent on the alias and on bare `/graphql`,
    // where `resolve_principal` falls back to the sole registered company.
    let addressed = company.map(|axum::extract::Path(id)| CompanyId::new(id));
    let auth = match resolve_principal(&headers, &state, addressed.as_ref(), peer).await {
        Ok(auth) => auth,
        Err(_) => {
            let err = async_graphql::ServerError::new("unauthorized", None);
            return async_graphql::Response::from_errors(vec![err]).into();
        }
    };
    let request = req.into_inner().data(state.clone()).data(auth);
    state.schema().execute(request).await.into()
}

/// `GET /graphql` — a minimal embedded GraphiQL explorer.
async fn graphiql() -> impl IntoResponse {
    Html(async_graphql::http::graphiql_source("/graphql", None))
}

/// Milliseconds in one UTC day.
const MILLIS_PER_DAY: u64 = 86_400_000;

/// The current wall-clock time in epoch millis (UTC).
pub(crate) fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The `(year, month, day)` of an epoch day, via Hinnant's public-domain
/// `civil_from_days`. Kept local so the read plane needs no date dependency.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

/// Formats epoch-millis as an RFC-3339 / ISO-8601 UTC timestamp (second
/// precision), the string form the console's `updatedAt`/`at` fields use.
pub(crate) fn iso8601(at_millis: u64) -> String {
    let (y, m, d) = civil_from_days((at_millis / MILLIS_PER_DAY) as i64);
    let secs = (at_millis % MILLIS_PER_DAY) / 1000;
    let (h, min, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
}

#[cfg(test)]
mod test;

/// What a page authored by an agent can reach when its `oc:graphql` request is
/// bridged to this handler.
///
/// The console's page bridge forwards the request under the **operator's own
/// authenticated session** — it narrows nothing per page. So the only things
/// standing between an agent-authored page and the operator's whole read plane
/// are properties of this module: [`EmptyMutation`], which is why a bridged
/// document can never write, and [`GqlAuth::authorize`], which is why it can
/// never read across companies. Both were load-bearing and neither was
/// asserted here.
#[cfg(test)]
mod bridge_scope_test {
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::ports::CompanyStore;
    use crate::ports::types::{CompanyId, CompanyRecord};
    use crate::runtime::RuntimeBuilder;
    use crate::server::router;
    use crate::server::test_support::{fixed_cookie, seed_fixed_admin};
    use crate::store::FsCompanyStore;
    use crate::{AppConfig, AppState};

    /// Two companies on one host, each with its own signed-in admin — the shape
    /// a per-page scope question needs, since a single-company host cannot tell
    /// "narrowed correctly" from "there was nothing else to reach".
    async fn state_with_two_companies(home: &std::path::Path) -> AppState {
        let store = FsCompanyStore::new(home.to_path_buf());
        let state = AppState::new(AppConfig::default()).with_home(home.to_path_buf());
        for name in ["acme", "globex"] {
            let id = CompanyId::new(name);
            let manifest = super::test::manifest();
            store
                .save(&CompanyRecord {
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
            state.registry().insert(id, Arc::new(runtime));
            seed_fixed_admin(&state, name).await;
        }
        state
    }

    async fn query_as(app: &axum::Router, cookie: &str, body: &str) -> serde_json::Value {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/graphql")
                    .header("content-type", "application/json")
                    .header("cookie", cookie)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// AUTH. A bridged request carries the operator's full session, so what a
    /// page may do is decided entirely here: it may not write, because the
    /// schema has no mutation root, and it may not read another company,
    /// because [`GqlAuth::authorize`] refuses a user principal any company but
    /// their own.
    ///
    /// The mutation is a *syntactically valid* document, so the only thing that
    /// can refuse it is the absent mutation root — a parse error would prove
    /// nothing.
    #[tokio::test]
    async fn a_bridged_request_can_neither_write_nor_read_another_company() {
        let home_dir = tempfile::tempdir().unwrap();
        let state = state_with_two_companies(home_dir.path()).await;
        let app = router(state);
        let acme = fixed_cookie("acme");

        let value = query_as(&app, &acme, r#"{"query":"mutation { __typename }"}"#).await;
        assert!(
            value["data"].is_null(),
            "a bridged page executed a mutation: {value}"
        );
        let errors = value["errors"].as_array().expect("an errors array");
        assert!(
            errors.iter().any(|e| e["message"]
                .as_str()
                .is_some_and(|m| m.to_ascii_lowercase().contains("mutation"))),
            "the refusal must be the absent mutation root, got {value}"
        );

        let value = query_as(
            &app,
            &acme,
            r#"{"query":"{ company(id: \"globex\") { id } }"}"#,
        )
        .await;
        assert!(
            value["data"]["company"].is_null(),
            "a bridged page read another company: {value}"
        );
        assert_eq!(
            value["errors"][0]["extensions"]["code"], "forbidden",
            "reaching another company must be refused as forbidden, got {value}"
        );

        let value = query_as(&app, &acme, r#"{"query":"{ companies { id } }"}"#).await;
        let ids: Vec<&str> = value["data"]["companies"]
            .as_array()
            .expect("a companies array")
            .iter()
            .map(|c| c["id"].as_str().expect("an id"))
            .collect();
        assert_eq!(
            ids,
            vec!["acme"],
            "the roster must not disclose that another company exists: {value}"
        );
    }

    /// CONC. The schema is built once at startup and shared by every request
    /// ([`build_schema`]); only the principal is per-request, injected as
    /// request data by [`graphql_handler`]. So a bridged page opened by one
    /// operator and a console open as another are executing against the same
    /// object at the same time, and the thing that must not be shared is the
    /// principal.
    ///
    /// Interleaved on purpose: alternating callers, all in flight together,
    /// each of which must see only its own company.
    #[tokio::test]
    async fn concurrent_requests_never_borrow_another_callers_principal() {
        let home_dir = tempfile::tempdir().unwrap();
        let state = state_with_two_companies(home_dir.path()).await;
        let app = router(state);

        let pending = (0..24).map(|i| {
            let company = if i % 2 == 0 { "acme" } else { "globex" };
            let app = app.clone();
            async move {
                let value = query_as(
                    &app,
                    &fixed_cookie(company),
                    r#"{"query":"{ companies { id } }"}"#,
                )
                .await;
                (company, value)
            }
        });

        for (company, value) in futures::future::join_all(pending).await {
            let ids: Vec<&str> = value["data"]["companies"]
                .as_array()
                .expect("a companies array")
                .iter()
                .map(|c| c["id"].as_str().expect("an id"))
                .collect();
            assert_eq!(
                ids,
                vec![company],
                "a concurrent request answered under another caller's principal: {value}"
            );
        }
    }
}
