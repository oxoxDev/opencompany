//! The toolkit catalog behind Composio **open mode** (issue #397).
//!
//! An empty `[tools.composio].toolkits` means "defer to the backend's
//! server-enforced allowlist" — every toolkit the backend permits, which today
//! is roughly a hundred slugs. Until this module existed the console was handed
//! a hardcoded eight instead and the backend's own catalog
//! (`GET /agent-integrations/composio/toolkits`) was never consulted, so the
//! list drifted the moment the backend added an integration.
//!
//! ## What this module owns
//!
//! * the answer for open mode — the fetched catalog, or an honestly-marked
//!   fallback;
//! * the process-level cache that keeps a page-load path off the network; and
//! * [`FALLBACK_TOOLKITS`], the last-resort list, which is **never** presented
//!   as if it were the real catalog.
//!
//! ## Why a fallback still exists
//!
//! The fetch needs a credential and a reachable backend, and a build that
//! compiled the `composio` feature in. A company missing any of those still has
//! to see something it can click — a console that renders nothing is the exact
//! failure #397 was filed about. So the fallback stays, but
//! [`CatalogSource::Fallback`] and a plain-language notice ride along with it so
//! the console can tell the operator the list may be incomplete. A fallback
//! served as though it were the catalog would be worse than no fallback: it
//! looks authoritative and is silently eight items long.
//!
//! ## This is a console affordance, not a capability change
//!
//! Nothing here widens or narrows what an agent may reach. Agent-side admission
//! is [`toolkit_allowed`](crate::harness::composio), which is untouched: an
//! empty manifest allowlist still admits every toolkit, a non-empty one still
//! admits only its members. Equally, a **non-empty** manifest allowlist never
//! consults this module at all — a company that deliberately narrowed its belt
//! is offered exactly what it chose, and the catalog cannot widen it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use serde::Serialize;

use crate::company::composio::CatalogEntry;

/// The last-resort provider list, used only when the real catalog cannot be
/// fetched — and always accompanied by [`CatalogSource::Fallback`] plus a
/// notice, so it is never mistaken for the backend's answer.
///
/// These are the toolkits a company reaches for first and are all in the
/// backend's default allowlist, which makes them a defensible thing to show
/// when the alternative is an empty page. They are **not** a ceiling: the
/// console's free-text slug field still authorizes anything the backend
/// permits, and agent-side admission never consulted this list.
pub(crate) const FALLBACK_TOOLKITS: &[&str] = &[
    "gmail",
    "googlecalendar",
    "googledrive",
    "github",
    "slack",
    "notion",
    "linear",
    "discord",
];

/// How long a successfully fetched catalog is served before it is re-fetched.
///
/// Fifteen minutes. The catalog changes when Composio adds an integration or
/// the backend widens its allowlist — churn measured in days, not seconds. The
/// cost of staleness is that a brand-new provider is missing from a picker for
/// at most a quarter of an hour; the cost of no cache is a backend round-trip
/// on *every* console status poll, on a page-load path, for a list that will be
/// byte-identical. Fifteen minutes puts the ceiling at four fetches an hour per
/// company however hard the console polls.
pub(crate) const CATALOG_TTL: Duration = Duration::from_secs(15 * 60);

/// How long a *failed* fetch is remembered before another is attempted.
///
/// One minute — deliberately much shorter than [`CATALOG_TTL`]. A failure is
/// cached at all because a backend that is down would otherwise be dialled once
/// per status poll, adding a whole `FETCH_TIMEOUT` to every console page-load for as
/// long as the outage lasts. It is cached only briefly because the operator
/// staring at a degraded notice wants it to clear as soon as the backend
/// recovers, and a minute is short enough that a refresh feels like it worked.
pub(crate) const FAILURE_TTL: Duration = Duration::from_secs(60);

/// How long the status route waits on the catalog before degrading.
///
/// The shared integration client's own timeout is 60s, which is a sane budget
/// for an agent tool and a catastrophe here: this call sits on `GET …/composio`,
/// which the console fetches on page load and after every mutation. Five
/// seconds is long enough for a healthy backend and short enough that a sick one
/// costs the operator a visible notice rather than a hung panel.
///
/// Gated with its only caller: a build without the `composio` feature has no
/// client to time out.
#[cfg(feature = "composio")]
pub(crate) const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Where the toolkit list the console renders actually came from.
///
/// The console needs this told to it rather than inferred — the whole shape of
/// #397 is that an inferred answer was wrong in the one case that mattered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CatalogSource {
    /// The company's own `[tools.composio].toolkits`, offered verbatim. The
    /// catalog is not consulted and cannot widen it.
    Manifest,
    /// The backend's live toolkit catalog. This is the answer open mode is
    /// supposed to give.
    Backend,
    /// [`FALLBACK_TOOLKITS`] — the catalog could not be fetched. Always paired
    /// with a notice saying so.
    Fallback,
}

/// The toolkits open mode offers, and how honest the answer is.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpenModeToolkits {
    /// The providers to offer, with whatever display metadata the backend
    /// published for each (issue #600). A manifest or fallback list carries
    /// slugs only; a fetched catalog carries names, logos, descriptions and
    /// categories too.
    pub toolkits: Vec<CatalogEntry>,
    /// Where they came from.
    pub source: CatalogSource,
    /// Plain-language reason the list is a fallback, for the console to show the
    /// operator. `None` exactly when [`Self::source`] is not
    /// [`CatalogSource::Fallback`].
    pub notice: Option<String>,
}

impl OpenModeToolkits {
    /// The good case: the backend answered.
    pub(crate) fn from_backend(toolkits: Vec<CatalogEntry>) -> Self {
        Self {
            toolkits,
            source: CatalogSource::Backend,
            notice: None,
        }
    }

    /// The honest degradation: the built-in list, marked as such, with the
    /// reason attached.
    ///
    /// Slug-only entries, and unavoidably so: a fallback exists precisely
    /// because the metadata could not be fetched. The console renders these
    /// with its own typography, which is why that fallback survives #600 rather
    /// than being deleted in favour of the backend's names.
    pub(crate) fn degraded(reason: &str) -> Self {
        Self {
            toolkits: FALLBACK_TOOLKITS
                .iter()
                .map(|s| CatalogEntry::from_slug(*s))
                .collect(),
            source: CatalogSource::Fallback,
            notice: Some(format!(
                "Composio's provider catalog could not be fetched ({}), so this is a built-in \
                 starter list and may be incomplete. Any other provider the backend permits can \
                 still be connected by its slug.",
                bound_reason(reason)
            )),
        }
    }

    /// Turn a cached-or-fresh fetch outcome into the rendered answer.
    pub(crate) fn from_outcome(outcome: Result<Vec<CatalogEntry>, String>) -> Self {
        match outcome {
            Ok(toolkits) => Self::from_backend(toolkits),
            Err(reason) => Self::degraded(&reason),
        }
    }

    /// Just the slugs, in render order — the wire field the console has always
    /// had and every existing consumer still reads.
    ///
    /// Kept as a derived view rather than a second stored list so the two can
    /// never disagree about which providers are on offer.
    pub(crate) fn slugs(&self) -> Vec<String> {
        self.toolkits.iter().map(|t| t.slug.clone()).collect()
    }
}

/// Keep an upstream reason to one readable clause.
///
/// The upstream text has already had the tenant bearer stripped out of it by
/// the harness before it reaches here; this is a legibility bound, not a
/// security one. A backend that answers with a wall of HTML should not push a
/// wall of HTML into a status response.
fn bound_reason(reason: &str) -> String {
    const MAX: usize = 160;
    let reason = reason.trim().replace(['\n', '\r'], " ");
    if reason.is_empty() {
        return "no reason given".to_string();
    }
    match reason.char_indices().nth(MAX) {
        None => reason,
        Some((cut, _)) => format!("{}…", &reason[..cut]),
    }
}

/// One cached fetch outcome and when it was recorded.
struct CacheEntry {
    at: Instant,
    outcome: Result<Vec<CatalogEntry>, String>,
}

impl CacheEntry {
    /// Successes live for [`CATALOG_TTL`], failures for [`FAILURE_TTL`].
    fn ttl(&self) -> Duration {
        if self.outcome.is_ok() {
            CATALOG_TTL
        } else {
            FAILURE_TTL
        }
    }
}

/// A process-level, per-company cache of the fetched catalog.
///
/// Keyed per company rather than per backend URL even though the catalog is
/// mostly a property of the backend: a company's own BYO token can resolve to a
/// different Composio account with a different answer, and sharing one entry
/// across tenants would let one company's outage mark another company's panel
/// degraded. Entries are small (a hundred short strings) and bounded by the
/// number of companies an instance hosts.
/// What a catalog fetch produced: the entries, or a plain-language reason.
type FetchOutcome = Result<Vec<CatalogEntry>, String>;

#[derive(Default)]
pub(crate) struct CatalogCache {
    entries: Mutex<HashMap<String, CacheEntry>>,
    /// The fetches running right now, keyed the same way, so callers arriving
    /// on a cold key wait on one rather than each starting their own.
    in_flight: Mutex<HashMap<String, broadcast::Sender<FetchOutcome>>>,
}

/// Clears `key` from [`CatalogCache::in_flight`] however the leader's fetch
/// ends. A panic or a dropped request future must not leave an entry behind
/// that later callers would wait on forever.
struct Flight<'a> {
    cache: &'a CatalogCache,
    key: &'a str,
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        if let Ok(mut flights) = self.cache.in_flight.lock() {
            flights.remove(self.key);
        }
    }
}

impl CatalogCache {
    /// The catalog for `key`: the cached outcome, the answer of a fetch already
    /// running for the same key, or — for exactly one caller — `fetch`.
    ///
    /// The middle arm is what this adds. `GET …/composio` sits on a page-load
    /// path the console asks more than once per paint, and on a cold key every
    /// concurrent caller used to dial the backend for a list they were certain
    /// to agree on, each paying the full [`FETCH_TIMEOUT`] before the page could
    /// paint.
    ///
    /// Coalescing is an optimisation and never a correctness dependency: a
    /// poisoned map, a leader that was cancelled, or a caller that arrives just
    /// as one finishes all fall through to a fetch of their own.
    pub(crate) async fn get_or_fetch<F, Fut>(
        &self,
        key: &str,
        fetch: F,
    ) -> Result<Vec<CatalogEntry>, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Vec<CatalogEntry>, String>>,
    {
        if let Some(cached) = self.lookup(key, Instant::now()) {
            return cached;
        }
        let joined = {
            let Ok(mut flights) = self.in_flight.lock() else {
                return fetch().await;
            };
            match flights.get(key) {
                Some(tx) => Err(tx.subscribe()),
                None => {
                    let (tx, _) = broadcast::channel(1);
                    flights.insert(key.to_string(), tx.clone());
                    Ok(tx)
                }
            }
        };
        let tx = match joined {
            Ok(tx) => tx,
            Err(mut rx) => {
                return match rx.recv().await {
                    Ok(outcome) => outcome,
                    // The leader went away without answering, or answered just
                    // before this caller subscribed. Its result is in the cache
                    // in the second case; in the first there is nothing to wait
                    // for any more.
                    Err(_) => match self.lookup(key, Instant::now()) {
                        Some(outcome) => outcome,
                        None => fetch().await,
                    },
                };
            }
        };
        let _flight = Flight { cache: self, key };
        let outcome = fetch().await;
        self.store(key, outcome.clone(), Instant::now());
        let _ = tx.send(outcome.clone());
        outcome
    }

    /// The cached outcome for `key`, if one was recorded within its TTL of
    /// `now`. An expired entry reads as a miss and is left for the next
    /// [`Self::store`] to overwrite.
    pub(crate) fn lookup(
        &self,
        key: &str,
        now: Instant,
    ) -> Option<Result<Vec<CatalogEntry>, String>> {
        let entries = self.entries.lock().ok()?;
        let entry = entries.get(key)?;
        (now.saturating_duration_since(entry.at) < entry.ttl()).then(|| entry.outcome.clone())
    }

    /// Record an outcome as observed at `at`.
    pub(crate) fn store(&self, key: &str, outcome: Result<Vec<CatalogEntry>, String>, at: Instant) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(key.to_string(), CacheEntry { at, outcome });
        }
    }

    /// Drop `key`'s entry so the next read re-fetches.
    ///
    /// Called when the company's credential changes: a rotated BYO token can
    /// resolve to a different Composio account, and continuing to serve the old
    /// account's catalog for up to [`CATALOG_TTL`] would be exactly the kind of
    /// stale-by-construction answer this issue is about.
    pub(crate) fn evict(&self, key: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(key);
        }
    }
}

/// The process-wide catalog cache.
pub(crate) fn cache() -> &'static CatalogCache {
    static CACHE: OnceLock<CatalogCache> = OnceLock::new();
    CACHE.get_or_init(CatalogCache::default)
}

/// The cache key for a company on a backend. Both halves matter: the same
/// company reconfigured onto a different backend must not read the old
/// backend's catalog.
pub(crate) fn cache_key(company: &crate::ports::types::CompanyId, backend_url: &str) -> String {
    format!("{company}|{backend_url}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Slug-only catalog entries — what a manifest list, a fallback list, or a
    /// backend predating the dynamic catalog yields.
    fn slugs(list: &[&str]) -> Vec<CatalogEntry> {
        list.iter().map(|s| CatalogEntry::from_slug(*s)).collect()
    }

    /// A fetched catalog is served as the backend's answer, with nothing
    /// apologising for it.
    #[test]
    fn a_fetched_catalog_is_reported_as_the_backend_answer() {
        let resolved = OpenModeToolkits::from_outcome(Ok(slugs(&["gmail", "hubspot", "zendesk"])));
        assert_eq!(resolved.source, CatalogSource::Backend);
        assert_eq!(resolved.toolkits, slugs(&["gmail", "hubspot", "zendesk"]));
        assert_eq!(resolved.notice, None, "a real catalog needs no caveat");
    }

    /// The honesty requirement: a failed fetch still yields a usable list, but
    /// it is marked as a fallback AND says why, so the console can tell the
    /// operator the list may be incomplete. A fallback that reported
    /// `CatalogSource::Backend`, or carried no notice, would look exactly like
    /// the real catalog — which is the failure mode this test exists to stop.
    #[test]
    fn a_failed_fetch_degrades_visibly_and_says_why() {
        let resolved = OpenModeToolkits::from_outcome(Err("backend unreachable".to_string()));
        assert_eq!(resolved.source, CatalogSource::Fallback);
        assert_eq!(resolved.toolkits, slugs(FALLBACK_TOOLKITS));
        let notice = resolved.notice.expect("a fallback must explain itself");
        assert!(
            notice.contains("backend unreachable"),
            "the operator is told the actual reason: {notice}"
        );
        assert!(
            notice.contains("may be incomplete"),
            "the operator is told the list is not authoritative: {notice}"
        );
    }

    /// An upstream error that arrives as a wall of text is bounded before it
    /// reaches a status response.
    #[test]
    fn an_enormous_upstream_reason_is_bounded() {
        let resolved = OpenModeToolkits::from_outcome(Err("x".repeat(5_000)));
        let notice = resolved.notice.expect("fallback notice");
        assert!(
            notice.len() < 500,
            "unbounded reason: {} bytes",
            notice.len()
        );
        assert!(notice.contains('…'), "the cut is visible: {notice}");
    }

    /// Bounding is UTF-8 safe — a multi-byte reason must not panic on a byte
    /// slice through a codepoint.
    #[test]
    fn bounding_a_multibyte_reason_does_not_panic() {
        let bounded = bound_reason(&"é".repeat(500));
        assert!(bounded.ends_with('…'));
    }

    /// A newline-laden reason collapses to one clause, and an empty one still
    /// produces a sentence rather than an awkward gap.
    #[test]
    fn a_reason_is_normalised() {
        assert_eq!(bound_reason("  a\nb  "), "a b");
        assert_eq!(bound_reason("   "), "no reason given");
    }

    /// A stored success is served for `CATALOG_TTL` and not a moment longer.
    #[test]
    fn a_cached_catalog_expires_at_the_ttl() {
        let cache = CatalogCache::default();
        let at = Instant::now();
        cache.store("k", Ok(slugs(&["gmail"])), at);

        assert_eq!(
            cache.lookup("k", at + CATALOG_TTL - Duration::from_secs(1)),
            Some(Ok(slugs(&["gmail"]))),
            "inside the TTL the cache answers without a fetch"
        );
        assert_eq!(
            cache.lookup("k", at + CATALOG_TTL + Duration::from_secs(1)),
            None,
            "past the TTL the caller must re-fetch"
        );
    }

    /// A failure is remembered — so an outage does not cost a timeout per poll
    /// — but for a fraction of the success TTL, so recovery is visible quickly.
    #[test]
    fn a_cached_failure_expires_far_sooner_than_a_success() {
        let cache = CatalogCache::default();
        let at = Instant::now();
        cache.store("k", Err("boom".to_string()), at);

        assert!(
            FAILURE_TTL < CATALOG_TTL,
            "a remembered failure must not outlive a remembered success"
        );
        assert_eq!(
            cache.lookup("k", at + FAILURE_TTL - Duration::from_secs(1)),
            Some(Err("boom".to_string())),
            "a repeat poll during an outage is served from cache, not the network"
        );
        assert_eq!(
            cache.lookup("k", at + FAILURE_TTL + Duration::from_secs(1)),
            None,
            "a recovered backend is re-probed within the minute"
        );
    }

    /// An eviction forces the next read to re-fetch — the credential-rotation
    /// path.
    #[test]
    fn eviction_forces_a_refetch() {
        let cache = CatalogCache::default();
        let at = Instant::now();
        cache.store("k", Ok(slugs(&["gmail"])), at);
        cache.evict("k");
        assert_eq!(cache.lookup("k", at), None);
    }

    /// Two callers arriving on a cold key perform exactly ONE fetch, and both
    /// get its answer.
    ///
    /// This is the page-load case: the console asks `GET …/composio` more than
    /// once per paint, and before coalescing each ask dialled the backend for a
    /// list they were certain to agree on. `join!` polls the first future to its
    /// first await point before starting the second, so the second is guaranteed
    /// to arrive while the first is still fetching.
    #[tokio::test]
    async fn two_cold_callers_share_one_fetch() {
        let cache = CatalogCache::default();
        let fetches = AtomicUsize::new(0);
        let fetch = || async {
            fetches.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(slugs(&["gmail"]))
        };

        let (first, second) = tokio::join!(
            cache.get_or_fetch("k", fetch),
            cache.get_or_fetch("k", fetch)
        );

        assert_eq!(
            fetches.load(Ordering::SeqCst),
            1,
            "a second caller on a cold key must wait on the fetch in flight, not start another"
        );
        assert_eq!(first, Ok(slugs(&["gmail"])));
        assert_eq!(second, first, "both callers get the same answer");
        assert_eq!(
            cache.lookup("k", Instant::now()),
            Some(Ok(slugs(&["gmail"]))),
            "the shared fetch still fills the cache"
        );
    }

    /// A shared FAILURE is shared too — and cached under [`FAILURE_TTL`], so an
    /// outage costs one fetch for every caller in the window rather than one
    /// each.
    #[tokio::test]
    async fn two_cold_callers_share_one_failure() {
        let cache = CatalogCache::default();
        let fetches = AtomicUsize::new(0);
        let fetch = || async {
            fetches.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Err("the Composio backend did not answer".to_string())
        };

        let (first, second) = tokio::join!(
            cache.get_or_fetch("k", fetch),
            cache.get_or_fetch("k", fetch)
        );

        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        assert_eq!(
            first,
            Err("the Composio backend did not answer".to_string())
        );
        assert_eq!(second, first);
    }

    /// A cached key never reaches the fetch at all — coalescing is added in
    /// front of the cache, not in place of it.
    #[tokio::test]
    async fn a_cached_key_is_served_without_a_fetch() {
        let cache = CatalogCache::default();
        cache.store("k", Ok(slugs(&["slack"])), Instant::now());
        let fetches = AtomicUsize::new(0);

        let served = cache
            .get_or_fetch("k", || async {
                fetches.fetch_add(1, Ordering::SeqCst);
                Ok(slugs(&["gmail"]))
            })
            .await;

        assert_eq!(served, Ok(slugs(&["slack"])));
        assert_eq!(fetches.load(Ordering::SeqCst), 0);
    }

    /// Two companies fetching at once do not coalesce onto each other: the
    /// in-flight map is keyed exactly like the cache, so one tenant can never be
    /// served another's catalog.
    #[tokio::test]
    async fn different_keys_do_not_share_a_fetch() {
        let cache = CatalogCache::default();
        let fetches = AtomicUsize::new(0);
        let acme = || async {
            fetches.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(slugs(&["gmail"]))
        };
        let globex = || async {
            fetches.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(slugs(&["slack"]))
        };

        let (a, g) = tokio::join!(
            cache.get_or_fetch("acme", acme),
            cache.get_or_fetch("globex", globex)
        );

        assert_eq!(fetches.load(Ordering::SeqCst), 2);
        assert_eq!(a, Ok(slugs(&["gmail"])));
        assert_eq!(g, Ok(slugs(&["slack"])));
    }

    /// A key is released once its fetch is done, so the NEXT cold caller after
    /// an eviction fetches rather than waiting on a flight that already ended.
    #[tokio::test]
    async fn a_finished_flight_is_released() {
        let cache = CatalogCache::default();
        let fetches = AtomicUsize::new(0);
        let fetch = || async {
            fetches.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(slugs(&["gmail"]))
        };

        assert_eq!(cache.get_or_fetch("k", fetch).await, Ok(slugs(&["gmail"])));
        cache.evict("k");
        assert_eq!(cache.get_or_fetch("k", fetch).await, Ok(slugs(&["gmail"])));

        assert_eq!(
            fetches.load(Ordering::SeqCst),
            2,
            "an evicted key must re-fetch, not join a flight that has finished"
        );
    }

    /// Companies do not share an entry: one tenant's outage cannot mark another
    /// tenant's panel degraded, and a BYO token pointing at a different Composio
    /// account cannot leak its catalog sideways.
    #[test]
    fn companies_do_not_share_a_cache_entry() {
        use crate::ports::types::CompanyId;
        let url = "https://api.example.test";
        assert_ne!(
            cache_key(&CompanyId::new("acme"), url),
            cache_key(&CompanyId::new("globex"), url)
        );
        assert_ne!(
            cache_key(&CompanyId::new("acme"), url),
            cache_key(&CompanyId::new("acme"), "https://other.example.test")
        );
    }
}
