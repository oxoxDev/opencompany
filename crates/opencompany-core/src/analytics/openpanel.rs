//! The OpenPanel transport, and the one function that chooses a tracker.
//!
//! [`build`] is compiled into **every** build; `HttpOpenPanelTracker` is
//! compiled only under `--features analytics`. That split is the acceptance
//! criterion of issue #1739 expressed as a type rather than as a rule: in a
//! default build there is no type here that owns an HTTP client, so "a build
//! with no opt-in emits zero outbound analytics requests" is not a behaviour
//! that could regress — the code that would make the request is not in the
//! binary.
//!
//! Under the feature, [`build`] still returns [`NullTracker`] for every
//! [`Decision::Silent`], which is what a desktop or self-hosted install resolves
//! to. `a_self_hosted_build_makes_no_request` proves that against a real local
//! collector, and `a_hosted_tenant_reports` is its positive control — without
//! the second, a zero request count would be indistinguishable from a test that
//! never sends anything at all.
//!
//! # What is here, and what is deliberately not
//!
//! Everything about *what* an event says lives in [`crate::analytics::payload`],
//! [`Envelope`] and [`Event`], which are un-gated and tested in every lane. This
//! file owns only the HTTP call: how the body is delivered, how the credential
//! is presented, and what happens when the collector does not answer. Keeping
//! that line where it is is why a content leak would be caught by the default
//! `cargo test` rather than only by the one lane that compiles `reqwest`.
//!
//! # One request per event
//!
//! **OpenPanel has no batch endpoint.** `POST /track` takes a single
//! discriminated-union object; there is no array body and no `/batch` route, so
//! the batching this module used to do has nowhere to go. See [`Inner::drain`]
//! for what replaced it and why the queue survived the batch.

use std::sync::Arc;

use crate::analytics::config::Decision;
use crate::analytics::{Envelope, NullTracker, Tracker};

/// Chooses the tracker this process will use.
///
/// The whole of the "hosted tenants only, by default" decision lands here: a
/// [`Decision::Silent`] gets a [`NullTracker`], and in a build without the
/// `analytics` feature *every* decision does, because there is nothing else to
/// return.
///
/// # A transport that cannot be built is a [`NullTracker`], never a degraded one
///
/// [`HttpOpenPanelTracker::new`] is fallible because the HTTP client it wraps is
/// where the credential header and the send timeout are configured, and both
/// are load-bearing. The obvious fallback — `reqwest::Client::default()` — is
/// the wrong answer twice: that client carries **no default headers**, so every
/// request goes out unauthenticated and is refused, and it carries **no
/// timeout**, so a slow collector parks a drain forever and `Tracker::flush`
/// waits behind it, which is exactly the shutdown block the five-second bound
/// exists to prevent. It is also not even a safe fallback in the case that
/// produces it: `Client::default()` is `Client::new()`, which is
/// `ClientBuilder::new().build().expect(…)` — the same `build` that just
/// failed, now panicking at boot instead of returning an error.
///
/// So a client that will not build disables reporting and says so once, loudly.
/// Sending nothing is a documented outcome of this module with a whole
/// vocabulary of reasons behind it; sending unauthenticated requests with no
/// timeout is not.
pub fn build(decision: &Decision, envelope: Envelope) -> Arc<dyn Tracker> {
    match decision {
        Decision::Silent(_) => Arc::new(NullTracker),
        #[cfg(feature = "analytics")]
        Decision::Report {
            endpoint,
            credentials,
        } => match http::HttpOpenPanelTracker::new(endpoint, credentials, envelope) {
            Ok(tracker) => Arc::new(tracker),
            // Routed through the same redaction the send path uses. A builder
            // error carries no request URL today, but "the dependency does not
            // print one here" is not a property this crate owns, and the
            // endpoint on the same line comes from the one helper that redacts.
            Err(error) => {
                tracing::warn!(
                    endpoint = %crate::analytics::boot::loggable_endpoint(endpoint),
                    error = %http::loggable_send_error(error),
                    "[analytics] the HTTP client for the collector could not be built, so \
                     reporting is off for this process. Nothing will be sent."
                );
                Arc::new(NullTracker)
            }
        },
        // Without the feature there is no transport to hand back. Reporting was
        // configured and the build cannot honour it, which is worth one line at
        // boot: silently ignoring an explicit `OPENCOMPANY_ANALYTICS=on` is the
        // kind of quiet no-op an operator debugs for an hour.
        #[cfg(not(feature = "analytics"))]
        Decision::Report { .. } => {
            let _ = envelope;
            tracing::info!(
                "[analytics] reporting is configured but this build was compiled without \
                 the `analytics` feature, so nothing is sent"
            );
            Arc::new(NullTracker)
        }
    }
}

#[cfg(feature = "analytics")]
pub use http::HttpOpenPanelTracker;

/// The header OpenPanel takes the client id in.
///
/// Named here rather than inline because the gated tests assert the exact
/// spelling: a header the collector does not recognise is a 401 behind a
/// `debug!`, which is the silent failure this whole module is built around.
#[cfg(feature = "analytics")]
pub const CLIENT_ID_HEADER: &str = "openpanel-client-id";

/// Names this client on the operator's own collector.
///
/// OpenPanel stores `openpanel-sdk-name` / `openpanel-sdk-version` on the event,
/// which is how an operator running one collector for several things tells this
/// traffic apart from a browser SDK's. It costs one header and answers "what is
/// writing to my project?" without anyone having to ask us.
#[cfg(feature = "analytics")]
pub const SDK_NAME_HEADER: &str = "openpanel-sdk-name";

/// The version half of the pair above.
#[cfg(feature = "analytics")]
pub const SDK_VERSION_HEADER: &str = "openpanel-sdk-version";

/// The value sent as [`SDK_NAME_HEADER`].
#[cfg(feature = "analytics")]
pub const SDK_NAME: &str = "opencompany";

/// **Event names OpenPanel refuses outright.**
///
/// `packages/constants/index.ts`, read at commit
/// `3060ca10213693cf0385be2713c8743d16733a2b`. A `track` whose `payload.name` is
/// one of these fails the collector's own zod refinement and comes back 400.
///
/// Copied here rather than merely known, because the failure it guards is the
/// one this module is least able to notice: a rejected event is a `debug!` line
/// and nothing else, so a name collision introduced years from now would look
/// exactly like a healthy instance that happens to report one fewer event. The
/// test below is a compile-time-vocabulary check against a runtime constant, and
/// it costs nothing to keep.
///
/// It is not the whole of OpenPanel's name validation — `event-blocklist.ts`
/// also rejects names over 80 characters, names containing a newline, names
/// beginning `/`, and a long anti-abuse substring list (`${`, `%{`, `../`,
/// `union select`, …). Those are asserted alongside it rather than transcribed:
/// transcribing a fifty-entry blocklist is how a copy goes stale.
pub const OPENPANEL_RESERVED_EVENT_NAMES: [&str; 2] = ["session_start", "session_end"];

#[cfg(feature = "analytics")]
mod http {
    use std::sync::{Arc, Mutex, Weak};
    use std::time::Duration;

    use async_trait::async_trait;

    use crate::analytics::config::ClientCredentials;
    use crate::analytics::{Envelope, Event, Tracker, payload};

    /// How often the background task drains the queue.
    ///
    /// A threshold alone is not enough: a quiet instance would hold its events
    /// until the next one arrived, which on a company that ran two turns and
    /// stopped is forever.
    const FLUSH_INTERVAL: Duration = Duration::from_secs(30);

    /// The most events held before the oldest are dropped.
    ///
    /// Analytics must never be able to grow without bound inside a tenant
    /// container. If the collector is unreachable for long enough to fill this,
    /// the right outcome is losing telemetry, not the process.
    const MAX_QUEUED: usize = 500;

    /// How long a send may take before it is abandoned. Short on purpose:
    /// nothing waits on this, but a request that never completes is a task that
    /// never ends.
    const SEND_TIMEOUT: Duration = Duration::from_secs(5);

    /// Queues events and POSTs them to OpenPanel, one request each.
    pub struct HttpOpenPanelTracker {
        inner: Arc<Inner>,
    }

    struct Inner {
        /// Carries the client-id header as a **default header**, set once at
        /// construction and marked sensitive.
        ///
        /// This is the whole of the credential handling, and it is the part of
        /// the change worth reading twice. Mixpanel wanted its token stamped
        /// into every event's property bag, so the transport had to reach into
        /// a rendered payload and mutate it — which meant a captured body, a
        /// recorded event or a test fixture could carry the credential, and the
        /// only thing stopping it was that nothing did. OpenPanel authenticates
        /// with request headers, so the credential is set once, here, and never
        /// touches the body builder at all. There is no longer a code path that
        /// could put it in a payload.
        ///
        /// `HeaderValue::set_sensitive` on it, which keeps it out of
        /// `HeaderValue`'s own `Debug` and out of HPACK's shared table on
        /// HTTP/2.
        client: reqwest::Client,
        endpoint: String,
        /// Behind a lock because its cognition labels are re-read after boot
        /// — see [`Envelope::set_cognition`]. Only ever held to render one
        /// payload or to relabel, never across an await.
        envelope: std::sync::RwLock<Envelope>,
        queue: Mutex<Vec<serde_json::Value>>,
        /// Held for the whole of one `drain`, take **and** requests.
        ///
        /// Without it, the shutdown flush and the 30-second drain could
        /// overlap: the drain takes the entire queue and awaits its POSTs, the
        /// flush finds an empty queue, returns at once, and process exit
        /// cancels the requests still in flight. That loses events exactly when
        /// the collector is slow — the one case the graceful flush exists for.
        /// An **async** mutex because it is held across an await; the `queue`
        /// lock below stays a `std::sync` one and is never held across one.
        sending: tokio::sync::Mutex<()>,
        stop: tokio::sync::Notify,
        /// Whether the collector has already told us the credential is no good.
        ///
        /// A `401` is not a failure like the others. Every other thing that can
        /// go wrong here is transient — a collector restarting, a network
        /// blip — and deserves the `debug!` that #1739 settled on, because it
        /// resolves itself and a `warn!` per event would be a log flood for a
        /// problem nobody needs to act on. A refused credential resolves itself
        /// never: every event for the rest of the process's life is dropped, the
        /// boot line said "reporting to …", and the only trace is a `debug!` no
        /// operator has enabled. That is the exact failure this module exists to
        /// make impossible, arriving one layer below where the boot line can see
        /// it.
        ///
        /// So it is a `warn!`, and it is said **once**: the condition is
        /// permanent, so repeating it adds nothing and would drown the log of a
        /// busy tenant.
        credential_refused: std::sync::atomic::AtomicBool,
        /// Whether the collector has already answered with a redirect.
        ///
        /// The client follows none of them — see
        /// [`HttpOpenPanelTracker::new`] — which closes the credential leak and
        /// opens a diagnostic hole in its place: a `3xx` arrives here as an
        /// ordinary non-success response, so a misconfigured endpoint would
        /// look exactly like a collector rejecting every event, behind a
        /// `debug!` nobody has enabled, forever. That is the failure shape this
        /// module exists to refuse.
        ///
        /// So a redirect gets the [`Self::credential_refused`] treatment: it is
        /// a verdict on the *endpoint* rather than on one event, every event
        /// behind it gets the same one, and it is a `warn!` said exactly once.
        endpoint_redirects: std::sync::atomic::AtomicBool,
        /// How many events have been lost to a **cancelled** drain.
        ///
        /// Every other way a drain ends states its own count in its own log
        /// line. Cancellation cannot: the future is dropped, so there is no
        /// branch to log from. [`CancelledDrain`] reports it on `Drop` and
        /// records the total here, which makes the loss assertable — a log line
        /// alone is not something a test can hold to account.
        lost_to_cancellation: std::sync::atomic::AtomicUsize,
    }

    impl HttpOpenPanelTracker {
        /// Builds a tracker and starts its drain loop.
        ///
        /// The credential is validated for header-safety in
        /// [`crate::analytics::config::resolve`], which is why the
        /// `from_str` call here can fall back rather than fail: by the time a
        /// [`Decision::Report`](crate::analytics::config::Decision::Report)
        /// exists, the client id is printable ASCII with no space, which is a
        /// strict subset of what `HeaderValue` takes. The fallback is an empty
        /// header value, which the collector refuses with a 401 — a loud,
        /// bounded outcome rather than a panic at boot, for a branch that is
        /// unreachable given the check upstream.
        ///
        /// # Fallible, because there is no acceptable degraded client
        ///
        /// The client built here is the only place the credential header and
        /// [`SEND_TIMEOUT`] are set, so a client built without them is not a
        /// weaker version of this one — it is one that authenticates against
        /// nothing and can hang a shutdown. [`super::build`] turns the error
        /// into a `NullTracker` and one `warn!`; see the note there for why
        /// `reqwest::Client::default()` is not the fallback it looks like.
        ///
        /// # Redirects are never followed
        ///
        /// `reqwest`'s default policy follows up to ten hops, and its
        /// cross-origin sanitization removes only `Authorization`, `Cookie`,
        /// `cookie2`, `Proxy-Authorization` and `WWW-Authenticate`
        /// (`redirect.rs::remove_sensitive_headers`, reqwest 0.12.28, read
        /// rather than assumed). `openpanel-client-id` is none of those, so a `302` from the configured endpoint to any other
        /// authority — a reverse proxy sending unauthenticated callers to an
        /// SSO host is the ordinary way one arrives — would have handed this
        /// instance's write credential to a host the operator never named.
        /// `HeaderValue::set_sensitive` does not help: it governs `Debug` and
        /// HPACK indexing, not redirect handling.
        ///
        /// That sanitization also compares only **host and port**, never the
        /// scheme, so an `https` endpoint that redirected to `http://` on the
        /// same host would have carried the credential across in cleartext — the
        /// `Silence::InsecureEndpoint` rule in
        /// [`crate::analytics::config`] bypassed by a response the operator
        /// does not control.
        ///
        /// So: [`reqwest::redirect::Policy::none`], with no same-origin
        /// exception. A same-origin policy would also be safe, but it is a
        /// predicate to keep correct rather than an invariant to state, and all
        /// it buys is a collector that 301s `/track` to `/api/track` — an
        /// endpoint the operator can type correctly once, after reading the
        /// warning [`Inner::report_redirected_endpoint`] emits. Following none
        /// of them makes "the credential only ever goes to the configured
        /// endpoint" a property of this client rather than a claim about a
        /// comparison.
        ///
        /// # A cleartext endpoint never goes through a proxy
        ///
        /// The same hole as the redirect one, by a different route, and it
        /// invalidates the loopback exception rather than merely widening it.
        /// `resolve` permits plain `http` only for a loopback host, and the
        /// entire justification is that such a request **does not leave the
        /// host** — so there is no wire between machines for the credential to
        /// be read off. A proxy makes that false. `reqwest`'s builder defaults
        /// to `auto_sys_proxy: true` (`async_impl/client.rs:309`), which pushes
        /// `ProxyMatcher::system()`, and that reads `HTTP_PROXY`/`ALL_PROXY`
        /// with exclusions taken **only** from `NO_PROXY` — hyper-util 0.1.20's
        /// matcher has no implicit carve-out for `localhost` or `127.0.0.0/8`,
        /// checked rather than assumed. So on a host with `HTTP_PROXY` set and
        /// no matching `NO_PROXY`, `http://localhost:3000/track` was sent to the
        /// proxy instead, in cleartext, with the credential header on it.
        ///
        /// So the cleartext case builds with
        /// [`reqwest::ClientBuilder::no_proxy`], which makes "it does not leave
        /// the host" true by construction instead of by assumption about the
        /// operator's environment. That is the same move as
        /// `redirect::Policy::none()`: a security property should be a fact
        /// about this client, not a prediction about its surroundings.
        ///
        /// **`https` keeps its proxy support, deliberately.** A proxied `https`
        /// request is a `CONNECT` tunnel: the proxy learns the host and port and
        /// never sees a header, so the credential is not exposed to it, and
        /// egress-restricted networks genuinely need it to reach a collector at
        /// all. Disabling proxies outright would break those deployments to fix
        /// a leak they do not have.
        ///
        /// The scheme is the whole test, because by the time a
        /// [`Decision::Report`](crate::analytics::config::Decision::Report)
        /// exists, `http` **implies** loopback — `config::is_secure_endpoint`
        /// has already refused every other `http` endpoint.
        ///
        /// # Crate-private, because that implication is the invariant
        ///
        /// The sentence above is only true of endpoints that came through
        /// [`resolve`](crate::analytics::config::resolve). While this
        /// constructor was `pub` it was also a way around it: the type is
        /// re-exported from a `pub mod`, so an `analytics`-enabled caller could
        /// hand it `http://collector.internal/track` directly and get a tracker
        /// that posts the client id across a network in cleartext, with
        /// [`is_cleartext`] dutifully turning off the proxy on the way. A
        /// safety property enforced only by the route callers happen to take is
        /// the thing this module keeps arguing against, so the route is now the
        /// only one there is: [`super::build`] takes a `&Decision`, and a
        /// `Decision::Report` is what `resolve` produces.
        ///
        /// The `debug_assert!` is defence in depth against the same mistake
        /// arriving from *inside* the crate later. It calls
        /// `config::is_secure_endpoint` rather than restating the rule, because
        /// a second reader of a security predicate is a bypass waiting to be
        /// found — the same reason `is_usable_endpoint` refuses to hand-roll the
        /// URL grammar `reqwest` already parses.
        pub(crate) fn new(
            endpoint: &str,
            credentials: &ClientCredentials,
            envelope: Envelope,
        ) -> Result<Self, reqwest::Error> {
            debug_assert!(
                crate::analytics::config::is_secure_endpoint(endpoint),
                "a tracker was built for an endpoint the credential cannot safely cross; \
                 every endpoint must come through config::resolve"
            );
            let mut builder = reqwest::Client::builder()
                .timeout(SEND_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .default_headers(request_headers(credentials));
            if is_cleartext(endpoint) {
                builder = builder.no_proxy();
            }
            let inner = Arc::new(Inner {
                client: builder.build()?,
                endpoint: endpoint.to_string(),
                envelope: std::sync::RwLock::new(envelope),
                queue: Mutex::new(Vec::new()),
                sending: tokio::sync::Mutex::new(()),
                stop: tokio::sync::Notify::new(),
                credential_refused: std::sync::atomic::AtomicBool::new(false),
                endpoint_redirects: std::sync::atomic::AtomicBool::new(false),
                lost_to_cancellation: std::sync::atomic::AtomicUsize::new(0),
            });

            // A `Weak` so the loop cannot keep the tracker alive, and
            // `try_current` so constructing one outside a runtime is a
            // flush-only tracker rather than a panic. Neither is theoretical:
            // the drop path is how a rebuilt runtime retires its tracker, and a
            // synchronous test constructs one with no reactor.
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let weak = Arc::downgrade(&inner);
                handle.spawn(async move { drain_loop(weak).await });
            }

            Ok(Self { inner })
        }

        /// How many events a **cancelled** drain has lost so far.
        ///
        /// Exists so the shutdown-budget loss is assertable rather than merely
        /// logged: a `warn!` is what an operator sees, and a counter is what a
        /// test can hold to account. Every other way a drain ends already names
        /// its own count in its own line.
        #[cfg(test)]
        pub(super) fn lost_to_cancellation(&self) -> usize {
            self.inner
                .lost_to_cancellation
                .load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    /// Every header this client sends on every request: the client id, marked
    /// sensitive, and the two that name the client.
    pub(super) fn request_headers(credentials: &ClientCredentials) -> reqwest::header::HeaderMap {
        use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

        let sensitive = |raw: &str| {
            let mut value =
                HeaderValue::from_str(raw).unwrap_or_else(|_| HeaderValue::from_static(""));
            value.set_sensitive(true);
            value
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static(super::CLIENT_ID_HEADER),
            sensitive(credentials.expose_id()),
        );
        // Compile-time constants, both. They name this client on a collector the
        // operator may be pointing several things at.
        headers.insert(
            HeaderName::from_static(super::SDK_NAME_HEADER),
            HeaderValue::from_static(super::SDK_NAME),
        );
        if let Ok(version) = HeaderValue::from_str(env!("CARGO_PKG_VERSION")) {
            headers.insert(HeaderName::from_static(super::SDK_VERSION_HEADER), version);
        }
        headers
    }

    impl Drop for HttpOpenPanelTracker {
        fn drop(&mut self) {
            self.inner.stop.notify_waiters();
        }
    }

    impl std::fmt::Debug for HttpOpenPanelTracker {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            // No endpoint and certainly no credential: this type holds one, and
            // a `{:?}` in a log line is exactly how one escapes.
            f.write_str("HttpOpenPanelTracker")
        }
    }

    async fn drain_loop(weak: Weak<Inner>) {
        loop {
            let Some(inner) = weak.upgrade() else { return };
            let stopped = {
                let stop = &inner.stop;
                tokio::select! {
                    _ = stop.notified() => true,
                    _ = tokio::time::sleep(FLUSH_INTERVAL) => false,
                }
            };
            inner.drain().await;
            if stopped {
                return;
            }
        }
    }

    /// Reports the tail of a drain that was **cancelled** rather than finished.
    ///
    /// Cancellation is the one way [`Inner::drain`] can end without saying
    /// anything, because it is not a branch the drain takes — the future is
    /// dropped out from under it. In practice that means the shutdown flush
    /// running out of its budget (`server::shutdown::flush_budget`, at most 2s),
    /// which with one request per event is a routine occurrence for a busy
    /// tenant rather than an exotic one. Before this, those events vanished and
    /// the only trace was a `debug!` at the call site that names no count.
    ///
    /// `Drop` **is** the cancellation path, so the report lives there. Every
    /// deliberate exit disarms the guard first, because each of those logs its
    /// own count and a second line would double-count the same events.
    ///
    /// It never prints the raw endpoint — `loggable_endpoint` is applied when
    /// the guard is built, for the reason [`loggable_send_error`] exists.
    struct CancelledDrain<'a> {
        /// Events taken off the queue that have not been sent yet.
        remaining: usize,
        /// Already redacted at construction; a `Drop` impl is the last place to
        /// remember to redact something.
        endpoint: String,
        /// Bumped by the total lost, so the loss is observable and not merely
        /// logged — a test can assert it without standing up a subscriber.
        lost: &'a std::sync::atomic::AtomicUsize,
    }

    impl CancelledDrain<'_> {
        /// The drain ended on a path that reports for itself.
        fn disarm(&mut self) {
            self.remaining = 0;
        }
    }

    impl Drop for CancelledDrain<'_> {
        fn drop(&mut self) {
            if self.remaining == 0 {
                return;
            }
            self.lost
                .fetch_add(self.remaining, std::sync::atomic::Ordering::Relaxed);
            // `warn!` rather than `debug!`, and this is the one place in the
            // module where that is not the transient/permanent rule at work.
            // It is bounded — a drain is cancelled at most once per shutdown —
            // and it is the only notice an operator gets that their restarts
            // are costing them the end of every session's telemetry. A `debug!`
            // here would be the same silence the count was added to break.
            tracing::warn!(
                endpoint = %self.endpoint,
                dropped = self.remaining,
                "[analytics] the drain was cancelled before it finished — almost always \
                 the shutdown flush running out of its budget. These events are lost. \
                 OpenPanel has no batch endpoint, so a queue costs one request per \
                 event; a collector that answers slowly, or a busy queue, will not fit \
                 the budget."
            );
        }
    }

    /// Whether `endpoint` is a plain `http` URL, and so one whose safety rests
    /// on the request never leaving the host.
    ///
    /// Parsed with `url` rather than matched on a `http://` prefix, for the
    /// reason `config::is_usable_endpoint` gives at length: the transport's own
    /// parser is the only one whose answer is the operative one, and `HTTP://`
    /// is a legal spelling that a prefix match reads as safe.
    ///
    /// A value that does not parse answers `false`, which is the harmless
    /// direction *here* — it can only leave the system proxy enabled for an
    /// endpoint that `resolve` has already refused to report to, so no request
    /// is ever built from it.
    pub(super) fn is_cleartext(endpoint: &str) -> bool {
        url::Url::parse(endpoint).is_ok_and(|parsed| parsed.scheme() == "http")
    }

    /// Whether `status` is the collector's answer about **itself** rather than
    /// about the event that happened to be in flight.
    ///
    /// Three statuses reach the drain that are not per-event verdicts, and they
    /// split by whether they resolve on their own. A `401` and a `3xx` are
    /// permanent misconfigurations, so each gets its own said-once `warn!`.
    /// These are the transient half: `429` is the collector or its proxy asking
    /// for less traffic, and a `5xx` is it failing to serve at all. Neither says
    /// anything about the body that was posted, so every event behind it in the
    /// queue would get the same answer.
    ///
    /// Without this the drain treated them as a rejected *event* and carried on,
    /// which is the worst available response to `503`: up to [`MAX_QUEUED`]
    /// requests aimed at a service that has just said it is overloaded, and
    /// again at the next [`FLUSH_INTERVAL`], for as long as the collector stays
    /// down. That is the same runaway [`Inner::report_refused_credential`] was
    /// added to stop, arriving from the transient direction — an analytics
    /// client should not be the thing that keeps an operator's collector down.
    ///
    /// **`408` and `425` are deliberately not here.** Both are arguably
    /// retryable, but neither is evidence the collector is unwell, and widening
    /// this predicate costs a whole drain each time it is wrong. `4xx` other
    /// than `401` and `429` stays per-event, which is the reading that loses the
    /// least when it is mistaken: one dropped event rather than a whole drain.
    pub(super) fn is_collector_wide(status: reqwest::StatusCode) -> bool {
        status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
    }

    /// The one rendering of a transport failure this module is allowed to log.
    ///
    /// `reqwest::Error` keeps the request URL and prints it — `… for url (…)` —
    /// and that URL is `OPENCOMPANY_ANALYTICS_ENDPOINT`. A self-hosted collector
    /// is routinely reached through an authenticated proxy, and that is
    /// precisely where the proxy's key lives: in userinfo
    /// (`https://user:key@host/track`) or in the query string (`?key=…`). So a
    /// collector that merely goes unreachable wrote the operator's credential
    /// into container logs, on a path the boot line's redaction never touched
    /// and the `ClientCredentials` redaction guards different strings from
    /// entirely.
    ///
    /// `without_url` **removes** the URL rather than rewriting it, which is why
    /// this is not a second redaction surface to keep in step with
    /// `boot::loggable_endpoint`. There is nothing here to diverge: the error
    /// carries no URL at all, and the destination on the same log line comes
    /// from that one helper, so the transport learns about a new place a URL can
    /// hold a secret at the same moment the boot line does.
    pub(super) fn loggable_send_error(error: reqwest::Error) -> String {
        error.without_url().to_string()
    }

    impl Inner {
        /// Takes everything queued and posts it, **one request per event**.
        /// Every failure is swallowed after one debug line: a dead collector is
        /// a no-op, per #1739's constraints.
        ///
        /// Serialized: a caller entering while another drain is in flight waits
        /// for it and then takes whatever has arrived since. That is what makes
        /// [`Tracker::flush`] a real guarantee rather than a queue inspection —
        /// see [`Inner::sending`].
        ///
        /// # Why the queue survived the loss of batching
        ///
        /// The obvious reading of "OpenPanel has no batch endpoint" is to drop
        /// the queue and fire a request from `track` itself. That is the wrong
        /// trade twice over. `track` is called from the cycle bracket and the
        /// usage meter, both on a turn's hot path, and it is **synchronous and
        /// infallible** by contract — it cannot await, so firing from it means
        /// spawning a task per event, which is unbounded concurrency against a
        /// collector the process does not control, with no back-pressure and no
        /// ceiling on memory. The queue is what bounds both: at most
        /// [`MAX_QUEUED`] events exist at once, and at most one drain runs at a
        /// time.
        ///
        /// # The unreachable collector, and why the drain gives up early
        ///
        /// Each request has its own [`SEND_TIMEOUT`]. Sequentially, a full
        /// queue against a black-holing collector would be
        /// `500 × 5s` — over forty minutes of a task doing nothing but proving
        /// the same thing five hundred times, and forty minutes in which the
        /// shutdown flush would block behind [`Inner::sending`] until its own
        /// budget cut it off. So a **transport** error abandons the rest of the
        /// drain: the collector is down, the remaining events are going
        /// nowhere, and the next interval will try again with whatever has
        /// accumulated since. They are dropped rather than requeued, because
        /// requeuing an unbounded backlog is how a bounded queue stops being
        /// bounded.
        ///
        /// An **HTTP status** failure does not abandon it. That is a per-event
        /// answer — a rejected event name, a payload the collector will not
        /// accept — and the events behind it may well be fine. Treating the two
        /// alike would let one malformed event silence a whole drain.
        ///
        /// **A `401` is the exception, because it is not a per-event answer at
        /// all.** It is the collector's verdict on this process's credential,
        /// so every event behind it in the queue will get the same one. Carrying
        /// on would fire up to [`MAX_QUEUED`] requests, every
        /// [`FLUSH_INTERVAL`], for the life of a misconfigured tenant — a
        /// thousand pointless requests a minute at the operator's own
        /// collector, to learn something already known. So it abandons the drain
        /// like a transport failure, and says so once.
        ///
        /// **A `3xx` is the same shape of exception, for the same reason.**
        /// This client follows no redirect at all — see
        /// [`HttpOpenPanelTracker::new`] for why the alternative hands the
        /// write credential to a host nobody configured — so a redirecting endpoint
        /// arrives here as a plain non-success response that will never
        /// resolve. It is a verdict on the endpoint, not on the event, so it
        /// abandons the drain and warns once rather than logging a `debug!` per
        /// event for the life of the process.
        ///
        /// # The tail a cancelled drain loses, and why it is said out loud
        ///
        /// Every path above ends the drain *deliberately* and says how many
        /// events it dropped. There is one that does not: the shutdown flush is
        /// wrapped in a [`tokio::time::timeout`] at its call site
        /// (`src/bin/opencompany.rs`, bounded by
        /// `server::shutdown::flush_budget`, at most **2s**), so when the budget
        /// runs out this future is simply **dropped mid-drain**. The events it
        /// had already taken out of the queue are gone, and nothing in this
        /// module ever said so — the only trace was a `debug!` at the call site
        /// that names no count.
        ///
        /// That gap is new with OpenPanel, and it is a direct consequence of
        /// there being no batch endpoint. Mixpanel's whole queue left in **one**
        /// request, so 2s was never the binding constraint; one request per
        /// event means a queue of `n` costs `n` round trips, and at a very
        /// ordinary 25 ms each the budget is spent after about eighty. A busy
        /// tenant restarting therefore loses the tail of its telemetry, quietly,
        /// on every rollout.
        ///
        /// [`CancelledDrain`] makes that loud instead. It is armed with the
        /// number of events still unsent, disarmed by every deliberate exit
        /// above (each of which logs its own line), and on `Drop` — which is
        /// what cancellation *is* — reports the count that never left.
        ///
        /// **The loss itself is not fixed here, on purpose.** The obvious
        /// remedy is to send with bounded concurrency, which would fit roughly
        /// `concurrency ×` more events into the same budget. It is declined
        /// because it is paid for out of the guarantee directly above: a drain
        /// that issues eight requests at once against a black-holing collector
        /// opens eight connections rather than one, and
        /// `an_unreachable_collector_costs_one_timeout_for_the_whole_drain`
        /// asserts exactly one. Trading a bounded shutdown for a multiplied
        /// hammering of a collector that is already unreachable is the wrong
        /// direction, and #1739 is explicit that telemetry loss beats a
        /// shutdown overrun — the budget exists because an overrun buys a
        /// `SIGKILL` mid-turn. The real fix is a batch endpoint on the
        /// collector, which OpenPanel does not have.
        async fn drain(&self) {
            let _sending = self.sending.lock().await;
            let events = {
                let mut queue = self.queue.lock().expect("analytics queue");
                if queue.is_empty() {
                    return;
                }
                std::mem::take(&mut *queue)
            };

            let total = events.len();
            // Armed for the whole loop. Every `return` below disarms it first,
            // because those paths log their own count; what is left for the
            // guard is the one exit that cannot log for itself — being dropped.
            let mut cancelled = CancelledDrain {
                remaining: total,
                endpoint: crate::analytics::boot::loggable_endpoint(&self.endpoint),
                lost: &self.lost_to_cancellation,
            };
            for (sent, event) in events.into_iter().enumerate() {
                cancelled.remaining = total - sent;
                match self.client.post(&self.endpoint).json(&event).send().await {
                    Ok(response) if response.status().is_success() => {}
                    // Not a per-event answer: the credential is wrong for every
                    // event behind this one too.
                    Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
                        cancelled.disarm();
                        self.report_refused_credential(total - sent);
                        return;
                    }
                    // Also not a per-event answer, and — because this client
                    // follows no redirects — not one that resolves itself.
                    Ok(response) if response.status().is_redirection() => {
                        cancelled.disarm();
                        self.report_redirected_endpoint(response.status(), total - sent);
                        return;
                    }
                    // Not a per-event answer either — but unlike the two above,
                    // this one resolves itself, so it gets the transient
                    // treatment rather than a `warn!`.
                    Ok(response) if is_collector_wide(response.status()) => {
                        cancelled.disarm();
                        tracing::debug!(
                            endpoint = %crate::analytics::boot::loggable_endpoint(&self.endpoint),
                            status = %response.status(),
                            dropped = total - sent,
                            "[analytics] the collector cannot take traffic right now; \
                             dropping the rest of this drain"
                        );
                        return;
                    }
                    Ok(response) => tracing::debug!(
                        status = %response.status(),
                        "[analytics] the collector refused an event; dropping it"
                    ),
                    Err(error) => {
                        cancelled.disarm();
                        tracing::debug!(
                            endpoint = %crate::analytics::boot::loggable_endpoint(&self.endpoint),
                            error = %loggable_send_error(error),
                            dropped = total - sent,
                            "[analytics] could not reach the collector; dropping the rest \
                             of this drain"
                        );
                        return;
                    }
                }
            }
            cancelled.disarm();
        }

        /// Says once, out loud, that the configured endpoint redirects and that
        /// nothing is being sent as a result.
        ///
        /// **Never prints the `Location` header.** It is a URL the collector
        /// chose, and a URL is the one place this module already knows a
        /// credential hides — an authenticated proxy's key lives in the
        /// userinfo or the query string, which is the whole reason
        /// [`loggable_send_error`] exists. A redirect target is *less* trusted
        /// than the configured endpoint, not more: the operator did not write
        /// it, and printing it verbatim would hand a hostile or merely careless
        /// collector a way to write arbitrary text into a tenant's logs. The
        /// status code alone is enough to act on, and the fix is in the
        /// operator's own environment file either way.
        fn report_redirected_endpoint(&self, status: reqwest::StatusCode, dropped: usize) {
            use std::sync::atomic::Ordering;
            if self.endpoint_redirects.swap(true, Ordering::Relaxed) {
                return;
            }
            tracing::warn!(
                endpoint = %crate::analytics::boot::loggable_endpoint(&self.endpoint),
                status = %status,
                dropped,
                "[analytics] the collector answered with a redirect, which this client \
                 never follows: the credential header would otherwise travel to a host \
                 OPENCOMPANY_ANALYTICS_ENDPOINT does not name. Every event will be \
                 dropped until that variable points at the collector directly. For a \
                 self-hosted OpenPanel behind its bundled Caddy that is \
                 https://<your-domain>/api/track."
            );
        }

        /// Says once, out loud, that the collector will not accept this
        /// process's credential. Never quotes it.
        fn report_refused_credential(&self, dropped: usize) {
            use std::sync::atomic::Ordering;
            if self.credential_refused.swap(true, Ordering::Relaxed) {
                return;
            }
            tracing::warn!(
                endpoint = %crate::analytics::boot::loggable_endpoint(&self.endpoint),
                dropped,
                "[analytics] the collector refused this instance's credential (401). \
                 Every event will be dropped until OPENCOMPANY_ANALYTICS_CLIENT_ID names \
                 a write client on that collector whose secret check is off (\"ignore \
                 CORS and secret\"). Note that OpenPanel requires the client id to be a \
                 UUIDv4."
            );
        }
    }

    #[async_trait]
    impl Tracker for HttpOpenPanelTracker {
        fn track(&self, event: Event) {
            let body = {
                let envelope = self.inner.envelope.read().expect("analytics envelope");
                payload(&envelope, &event)
            };
            let mut queue = self.inner.queue.lock().expect("analytics queue");
            if queue.len() >= MAX_QUEUED {
                queue.remove(0);
            }
            queue.push(body);
        }

        async fn flush(&self) {
            // Waits on any in-flight periodic drain before taking what is left,
            // so a shutdown overlapping the 30-second loop does not return while
            // the previous drain is still on the wire.
            self.inner.drain().await;
        }

        fn observe_cognition(&self, cognition: crate::ports::brain::Cognition) {
            self.inner
                .envelope
                .write()
                .expect("analytics envelope")
                .set_cognition(cognition);
        }
    }
}

#[cfg(all(test, feature = "analytics"))]
#[path = "openpanel_collector_tests.rs"]
mod tests_collector;
#[cfg(all(test, feature = "analytics"))]
#[path = "openpanel_transport_tests.rs"]
mod tests_transport;
