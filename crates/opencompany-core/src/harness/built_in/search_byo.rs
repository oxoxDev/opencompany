//! A company's **own** search connection: the BYO half of the search surface
//! (`issue #238` deferred it explicitly — "wiring it belongs with the console
//! credential surface that `composio` already uses").
//!
//! # What this inherits, and what it does not
//!
//! OpenHuman owns the search domain (`oh::search`): six engines, a canonical
//! `web_search_tool` slot, `managed` backend-proxied by default. The BYO engines
//! there — Brave, Exa, Querit, plus the standalone SearXNG tool — are ordinary
//! `Tool` implementations with public constructors that take a key and nothing
//! else. This module **calls those constructors** with the company's own stored
//! key. No provider trait, no HTTP client, no result parsing of its own: the
//! whole point is that a Brave result rendered for an OpenCompany agent is the
//! same text OpenHuman renders.
//!
//! What it does *not* do is call [`oh::search::build_search_tools`], for the
//! reason [`search`](crate::harness::search) already gives: that entry point
//! takes OpenHuman's global `Config`, and the harness assembles per-company
//! state instead of a process-wide config — two companies on one host search
//! through two different accounts, so a global is not merely awkward here, it is
//! wrong.
//!
//! # One name, whichever provider
//!
//! Every provider's canonical "search the web" tool is presented to the model as
//! **`web_search`** — the same name the managed surface uses — through
//! [`AliasedTool`]. A company that switches from managed to Brave changes what
//! the tool costs and who bills it; it does not change what the agent is told it
//! can do. The shipped research skills name `web_search` in their instructions,
//! and a belt where that name appears and disappears with a settings change is
//! how an agent comes to invent URLs instead of searching for them.
//!
//! Provider extras keep their upstream names (`exa_find_similar`,
//! `exa_get_contents`, `brave_news_search`, `brave_image_search`,
//! `brave_video_search`) — they are genuinely different affordances, and a name
//! borrowed from upstream is one an operator can look up.
//!
//! # Fail open to managed, never to nothing
//!
//! Resolution answers `None` when the company configured nothing, or configured
//! a provider whose credential is missing. The caller then wires the metered
//! managed surface, which is exactly what OpenHuman does ("a BYO engine with no
//! key falls back to the managed surface"). A half-configured settings page
//! therefore degrades to a working, capped search rather than to an agent with
//! no way to find a source.
//!
//! # Money, and why the daily cap does not follow
//!
//! The managed tool is metered and daily-capped because every call spends the
//! *platform's* money ([`search`](crate::harness::search) explains the ledger).
//! A BYO call spends the *company's* own account, billed by Brave or Exa
//! directly, under rate limits that company chose. Applying the platform's cap
//! to it would be this host throttling a bill it does not pay, so it does not:
//! the cap travels with the managed credential, and a company that wants a
//! ceiling on its own key sets one where the key is issued.

use std::sync::Arc;

use crate::company::search::{configuration_complete, provider_is_byo};
use crate::ports::SecretStore;
use crate::ports::types::CompanyId;

/// Results a BYO provider is asked for when the caller does not say. Matches the
/// managed tool's default so switching providers does not change how much
/// context one search costs.
const DEFAULT_MAX_RESULTS: usize = 5;

/// Seconds a BYO provider call may take before it is abandoned. Deliberately
/// shorter than a turn: a search that has not answered in half a minute has
/// already cost the agent more than the answer is worth.
const TIMEOUT_SECS: u64 = 30;

/// Language a SearXNG instance is queried in when the company sets none.
const SEARXNG_LANGUAGE: &str = "all";

/// One company's resolved BYO search connection.
///
/// Only ever constructed for a provider that is both BYO and complete — see
/// [`TenantSearch::resolve`]. `managed`, and every half-configured provider,
/// resolve to `None` rather than to a `TenantSearch` that would wire a tool with
/// no credential behind it.
#[derive(Clone)]
pub struct TenantSearch {
    provider: String,
    api_key: Option<String>,
    endpoint: Option<String>,
}

/// Prints the provider and endpoint, never the key.
impl std::fmt::Debug for TenantSearch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TenantSearch")
            .field("provider", &self.provider)
            .field("endpoint", &self.endpoint)
            .field(
                "api_key",
                &if self.api_key.is_some() {
                    "<redacted>"
                } else {
                    "<unset>"
                },
            )
            .finish()
    }
}

impl TenantSearch {
    /// Resolves a company's BYO search connection from its secret store.
    ///
    /// `Ok(None)` means "search through the managed surface": no provider
    /// stored, `managed` stored explicitly, an unknown slug, or a BYO provider
    /// whose credential half is missing. All four are ordinary states of a
    /// settings page, not errors.
    ///
    /// A store **read failure** is an `Err`, not `Ok(None)`. Collapsing them
    /// would make an unhealthy secret store indistinguishable from "not
    /// configured", and the caller's response differs: absence should fall back
    /// to managed, while a transient read error should keep the connection the
    /// roster already had. See `HarnessPool::resolve_tenant_search`.
    ///
    /// # Errors
    ///
    /// Returns an error when the secret store cannot be read.
    pub async fn resolve(
        secrets: &Arc<dyn SecretStore>,
        company: &CompanyId,
    ) -> crate::error::Result<Option<TenantSearch>> {
        // Asks the same resolver the console's status route and the
        // capabilities panel ask. That is not tidiness: the store's convergence
        // rule moves a credential from `search/api_key` to
        // `search/provider/<slug>/key` on the first save, and a reader still
        // looking at the flat address would see an unconfigured company and
        // silently drop it to managed search — the agents would keep searching,
        // they would just quietly stop using the account the operator pays for.
        // Every reader moves together or none does.
        let candidates = crate::company::search::candidates(company, secrets.as_ref()).await?;
        let marked =
            crate::company::search::store::load_default_slug(company, secrets.as_ref()).await?;
        let Some(active) = crate::company::search::resolve::active(&candidates, marked.as_deref())
        else {
            if !candidates.is_empty() {
                tracing::warn!(
                    company = %company,
                    "[search] a BYO provider is connected but none resolves; falling back to the \
                     managed surface"
                );
            }
            return Ok(None);
        };

        let provider = active.provider.slug.clone();
        // Belt to the resolver's braces: `active` only ever returns a complete,
        // enabled provider, and `managed` is never a record.
        if !provider_is_byo(&provider) {
            return Ok(None);
        }

        let api_key =
            crate::company::search::store::load_provider_key(company, secrets.as_ref(), &provider)
                .await?;
        let endpoint = active.provider.endpoint.clone();
        if !configuration_complete(&provider, api_key.is_some(), endpoint.is_some()) {
            tracing::warn!(
                company = %company,
                provider = %provider,
                "[search] BYO provider is selected but its credential is missing; falling back to \
                 the managed surface"
            );
            return Ok(None);
        }

        Ok(Some(TenantSearch {
            provider,
            api_key,
            endpoint,
        }))
    }

    /// The provider this company searches through. Never the key.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// A connection assembled directly, for tests that need one without a
    /// secret store behind it — the roster-build and `build_agent` tests, which
    /// are about what a *resolved* connection wires rather than about how it
    /// resolved.
    #[cfg(test)]
    pub fn for_test(provider: &str, api_key: Option<&str>, endpoint: Option<&str>) -> Self {
        Self {
            provider: provider.to_string(),
            api_key: api_key.map(str::to_string),
            endpoint: endpoint.map(str::to_string),
        }
    }

    /// A stable hash of the connection, for the roster staleness check.
    ///
    /// Covers the key as well as the provider and endpoint, so rotating a key
    /// with everything else unchanged still rebuilds the roster — otherwise a
    /// rotated credential would keep authenticating with the old one until a
    /// restart.
    pub fn fingerprint(config: &Option<TenantSearch>) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        match config {
            None => 0u8.hash(&mut hasher),
            Some(search) => {
                1u8.hash(&mut hasher);
                search.provider.hash(&mut hasher);
                search.api_key.hash(&mut hasher);
                search.endpoint.hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

pub use live::{BYO_SEARCH_TOOLS, byo_search_tools};

mod live {
    use super::{DEFAULT_MAX_RESULTS, SEARXNG_LANGUAGE, TIMEOUT_SECS, TenantSearch};

    use async_trait::async_trait;
    use serde_json::Value;

    use oh::search::tools::{
        BraveImageSearchTool, BraveNewsSearchTool, BraveVideoSearchTool, BraveWebSearchTool,
        ExaFindSimilarTool, ExaGetContentsTool, ExaSearchTool, QueritSearchTool, SearxngSearchTool,
    };
    use openhuman_core as oh;
    use tinytools::{
        PermissionLevel, Tool, ToolCallOptions, ToolCategory, ToolResult, ToolScope, ToolTimeout,
    };

    use crate::harness::search::WEB_SEARCH_TOOL;

    /// Every tool name a BYO provider can put on a belt, across all providers.
    ///
    /// Not "the names one company sees" — that is provider-dependent — but the
    /// closed set the `search` namespace has to account for, so
    /// [`namespace_of`](crate::harness::toolbelt::namespace_of) and the
    /// gateable-coverage invariant can be checked against it rather than against
    /// a list somebody remembered to update.
    pub const BYO_SEARCH_TOOLS: [&str; 6] = [
        "exa_find_similar",
        "exa_get_contents",
        "brave_news_search",
        "brave_image_search",
        "brave_video_search",
        WEB_SEARCH_TOOL,
    ];

    /// The search tools for one company's own provider connection.
    ///
    /// An unknown provider slug wires nothing and warns rather than failing the
    /// build: an agent that cannot search is a degraded agent, not a broken
    /// company. In practice the slug was validated by the console write route
    /// and again by [`TenantSearch::resolve`], so reaching the warn arm means
    /// somebody wrote the secret store directly.
    pub fn byo_search_tools(config: &TenantSearch) -> Vec<Box<dyn Tool>> {
        let key = config.api_key.clone();
        match config.provider.as_str() {
            "brave" => vec![
                alias(
                    BraveWebSearchTool::new(key.clone(), DEFAULT_MAX_RESULTS, TIMEOUT_SECS),
                    "Brave web search",
                ),
                Box::new(BraveNewsSearchTool::new(
                    key.clone(),
                    DEFAULT_MAX_RESULTS,
                    TIMEOUT_SECS,
                )),
                Box::new(BraveImageSearchTool::new(
                    key.clone(),
                    DEFAULT_MAX_RESULTS,
                    TIMEOUT_SECS,
                )),
                Box::new(BraveVideoSearchTool::new(
                    key,
                    DEFAULT_MAX_RESULTS,
                    TIMEOUT_SECS,
                )),
            ],
            "exa" => vec![
                alias(
                    ExaSearchTool::new(key.clone(), None, DEFAULT_MAX_RESULTS, TIMEOUT_SECS),
                    "Exa web search",
                ),
                Box::new(ExaFindSimilarTool::new(
                    key.clone(),
                    None,
                    DEFAULT_MAX_RESULTS,
                    TIMEOUT_SECS,
                )),
                Box::new(ExaGetContentsTool::new(
                    key,
                    None,
                    DEFAULT_MAX_RESULTS,
                    TIMEOUT_SECS,
                )),
            ],
            "querit" => vec![alias(
                QueritSearchTool::new(key, None, DEFAULT_MAX_RESULTS, TIMEOUT_SECS),
                "Querit web search",
            )],
            "searxng" => {
                // Resolution guarantees the endpoint for this provider; the
                // `unwrap_or_default` is the belt to that braces, and an empty
                // base URL makes the tool report an unreachable instance rather
                // than panic.
                let base_url = config.endpoint.clone().unwrap_or_default();
                vec![alias(
                    SearxngSearchTool::new(
                        base_url,
                        DEFAULT_MAX_RESULTS,
                        SEARXNG_LANGUAGE.to_string(),
                        TIMEOUT_SECS,
                    ),
                    "SearXNG web search",
                )]
            }
            other => {
                tracing::warn!(
                    provider = %other,
                    "[search] unknown BYO search provider stored; no search tools wired"
                );
                Vec::new()
            }
        }
    }

    /// Present `tool` to the model under OpenCompany's canonical
    /// [`WEB_SEARCH_TOOL`] name, with `label` as its operator-facing step
    /// label — the provider's name, since a BYO tool's engine is fixed at
    /// construction ("Exa web search"), matching the managed tool's branded
    /// label rather than the humanized alias name.
    fn alias(tool: impl Tool + 'static, label: &'static str) -> Box<dyn Tool> {
        Box::new(AliasedTool {
            inner: Box::new(tool),
            name: WEB_SEARCH_TOOL,
            label,
        })
    }

    /// One tool wearing a different name.
    ///
    /// Every other method delegates, so the aliased tool behaves exactly like
    /// the upstream one — including its schema, its permission level and its
    /// timeout policy. A method added to [`Tool`] upstream after this was
    /// written falls back to the trait default rather than the inner tool's
    /// override; the delegation list below is the thing to extend when that
    /// happens.
    struct AliasedTool {
        inner: Box<dyn Tool>,
        name: &'static str,
        label: &'static str,
    }

    #[async_trait]
    impl Tool for AliasedTool {
        fn name(&self) -> &str {
            self.name
        }

        // Not delegated: the inner tool's label names the upstream tool, and
        // the default would humanize the alias into a provider-less
        // "Web search".
        fn display_label(&self, _args: &Value) -> Option<String> {
            Some(self.label.to_string())
        }

        fn description(&self) -> &str {
            self.inner.description()
        }

        fn parameters_schema(&self) -> Value {
            self.inner.parameters_schema()
        }

        async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
            self.inner.execute(args).await
        }

        async fn execute_with_options(
            &self,
            args: Value,
            options: ToolCallOptions,
        ) -> anyhow::Result<ToolResult> {
            self.inner.execute_with_options(args, options).await
        }

        fn supports_markdown(&self) -> bool {
            self.inner.supports_markdown()
        }

        fn permission_level(&self) -> PermissionLevel {
            self.inner.permission_level()
        }

        fn permission_level_with_args(&self, args: &Value) -> PermissionLevel {
            self.inner.permission_level_with_args(args)
        }

        fn scope(&self) -> ToolScope {
            self.inner.scope()
        }

        fn category(&self) -> ToolCategory {
            self.inner.category()
        }

        fn is_concurrency_safe(&self, args: &Value) -> bool {
            self.inner.is_concurrency_safe(args)
        }

        fn external_effect(&self) -> bool {
            self.inner.external_effect()
        }

        fn external_effect_with_args(&self, args: &Value) -> bool {
            self.inner.external_effect_with_args(args)
        }

        fn max_result_size_chars(&self) -> Option<usize> {
            self.inner.max_result_size_chars()
        }

        fn timeout_policy(&self, args: &Value) -> ToolTimeout {
            self.inner.timeout_policy(args)
        }

        fn display_detail(&self, args: &Value) -> Option<String> {
            self.inner.display_detail(args)
        }
    }
}

#[cfg(test)]
#[path = "search_byo_tests.rs"]
mod tests;
