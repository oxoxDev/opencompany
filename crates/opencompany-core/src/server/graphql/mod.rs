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

    /// The skill registry (`companies/*/skills/*/SKILL.md`, the baseline's
    /// included), installable into any company. Unscoped — the registry is the
    /// same for every caller.
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

/// The current wall-clock time in epoch millis (UTC).
pub(crate) fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// `iso8601` (RFC-3339 formatting for the read plane's `updatedAt`/`at`
// fields) moved to `crate::ports::iso8601` (keys rework #2306, P3-7 review):
// a pure, dependency-free formatter belongs at the `ports` layer every layer
// can already reach, not under `server` — the account-key fan-out
// (`company::company_key::fan_out`) needed the same formatter, and
// `src/company/` must never import from `server`.

#[cfg(test)]
#[path = "graphql_test_group_1.rs"]
mod graphql_test_group_1;
#[cfg(test)]
#[path = "graphql_test_group_2.rs"]
mod graphql_test_group_2;
#[cfg(test)]
#[path = "graphql_test_group_3.rs"]
mod graphql_test_group_3;
#[cfg(test)]
#[path = "graphql_test_group_4.rs"]
mod graphql_test_group_4;
#[cfg(test)]
#[path = "graphql_test_support_1.rs"]
mod graphql_test_support_1;
#[cfg(test)]
#[path = "skills_drift_tests.rs"]
mod skills_drift_tests;

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
#[path = "graphql_bridge_scope_test.rs"]
mod bridge_scope_test;
