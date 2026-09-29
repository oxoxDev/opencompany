//! Collector-behavior tests (queueing, retries, cancellation, header
//! safety), split out of `openpanel_tests.rs` (topic split, >750 lines).
//! Shares the `spawn_collector`/`Collector`/`envelope`/`env` helpers,
//! duplicated from `openpanel_transport_tests.rs`.

use super::*;
use crate::analytics::Event;
use crate::analytics::config::{CLIENT_ID_ENV, ENDPOINT_ENV, resolve};
use crate::analytics::types::OpaqueId;
use crate::app::config::MapEnv;
use crate::app::deployment::{DEPLOYMENT_ENV, Deployment};
use crate::ports::brain::Cognition;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// An obviously-fake client id. Never a real one, in a file or anywhere else.
const TEST_CLIENT_ID: &str = "not-a-real-client-id";

/// A local collector that counts what it is sent.
struct Collector {
    hits: Arc<AtomicUsize>,
    url: String,
    shutdown: tokio::sync::oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

async fn spawn_collector() -> Collector {
    spawn_collector_with(Duration::ZERO, 0, axum::http::StatusCode::BAD_REQUEST).await
}

/// A collector that takes `delay` to answer and refuses the first
/// `refuse_first` requests with `refusal`, so a test can observe what
/// happens while a request is in flight, what happens after one event is
/// rejected, and what happens when the refusal is about the credential
/// rather than about the event.
async fn spawn_collector_with(
    delay: Duration,
    refuse_first: usize,
    refusal: axum::http::StatusCode,
) -> Collector {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen_hits = hits.clone();

    let app = axum::Router::new().route(
        "/track",
        axum::routing::post(
            move |_received: axum::http::HeaderMap,
                  axum::Json(_body): axum::Json<serde_json::Value>| {
                let hits = seen_hits.clone();
                async move {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    let seen = hits.fetch_add(1, Ordering::SeqCst);
                    if seen < refuse_first {
                        refusal
                    } else {
                        axum::http::StatusCode::OK
                    }
                }
            },
        ),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/track", listener.local_addr().unwrap());
    let (shutdown, rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
    });

    Collector {
        hits,
        url,
        shutdown,
        handle,
    }
}

impl Collector {
    async fn stop(self) {
        let _ = self.shutdown.send(());
        let _ = self.handle.await;
    }
}

fn envelope() -> Envelope {
    Envelope::new(
        OpaqueId::instance("0123456789abcdef0123456789abcdef"),
        Deployment::HostedTenant,
        Cognition::default(),
    )
}

/// A reporting environment pointed at `endpoint`, which `pairs` overrides.
fn env(endpoint: &str, pairs: &[(&str, &str)]) -> MapEnv {
    let mut all = vec![(CLIENT_ID_ENV, TEST_CLIENT_ID), (ENDPOINT_ENV, endpoint)];
    all.extend_from_slice(pairs);
    MapEnv::new(all)
}

/// **A shutdown flush waits for a send already in flight.**
///
/// The periodic drain takes the whole queue before it awaits its POSTs, so
/// a flush that only inspected the queue would find it empty, return at
/// once, and let process exit cancel the request carrying the event —
/// losing telemetry precisely when the collector is slow, which is the one
/// case the graceful flush exists for.
///
/// Asserted by timing, against a collector that takes 600ms: the second
/// flush must not return before the first request completes. The threshold
/// is 300ms against a 600ms delay, so it neither trips on scheduling jitter
/// nor passes without the wait (the unserialized version returns in
/// microseconds).
#[tokio::test]
async fn a_flush_waits_for_a_send_already_in_flight() {
    let collector =
        spawn_collector_with(Duration::from_millis(600), 0, axum::http::StatusCode::OK).await;
    let env = env(&collector.url, &[(DEPLOYMENT_ENV, "hosted-tenant")]);
    let tracker = build(&resolve(Deployment::from_env(&env), &env), envelope());

    tracker.track(Event::InstanceStarted {
        companies: 1,
        storage: "fs",
        setup_complete: true,
    });

    // Stands in for the 30-second drain loop: it takes the queue and is
    // then parked on the POST.
    let first = {
        let tracker = tracker.clone();
        tokio::spawn(async move { tracker.flush().await })
    };
    // Long enough for the spawned task to take the queue and start its
    // request, short enough to be well inside the 600ms the collector takes.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let started = Instant::now();
    tracker.flush().await;
    let waited = started.elapsed();

    assert!(
        waited >= Duration::from_millis(300),
        "the flush returned in {waited:?} while a send was still in flight; \
         on a real shutdown that event would be cancelled with the process"
    );

    first.await.expect("the in-flight send finished");
    assert_eq!(
        collector.hits.load(Ordering::SeqCst),
        1,
        "the event really was in flight and really did land"
    );
    collector.stop().await;
}

/// **A collector-wide status stops the drain, like a refused credential.**
///
/// `429`, `502`, `503` are the collector saying it cannot take traffic —
/// not a verdict on the body that happened to be in flight. Treating one as
/// a rejected *event* and carrying on is the worst available response: up
/// to `MAX_QUEUED` requests aimed at a service that has just said it is
/// overloaded, and again at the next `FLUSH_INTERVAL` for as long as it
/// stays down. An analytics client must not be the thing that keeps an
/// operator's own collector down.
///
/// The contrast with `a_refused_event_does_not_stop_the_drain` is the whole
/// point, and it is the same contrast a `401` draws: same collector, same
/// three events, one status code apart, opposite behaviour.
#[tokio::test]
async fn a_collector_that_cannot_take_traffic_stops_the_drain() {
    for refusal in [
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        axum::http::StatusCode::BAD_GATEWAY,
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        let collector = spawn_collector_with(Duration::ZERO, usize::MAX, refusal).await;
        let env = env(&collector.url, &[(DEPLOYMENT_ENV, "hosted-tenant")]);
        let tracker = build(&resolve(Deployment::from_env(&env), &env), envelope());

        for _ in 0..3 {
            tracker.track(Event::InstanceStarted {
                companies: 1,
                storage: "fs",
                setup_complete: true,
            });
        }
        tracker.flush().await;

        assert_eq!(
            collector.hits.load(Ordering::SeqCst),
            1,
            "{refusal} is the collector's answer about itself, so the two events \
             behind it must not be attempted"
        );
        collector.stop().await;
    }
}

/// The control for the test above: a status that really *is* about one
/// event must still not stop the drain.
///
/// Without it, "stops the drain" would be satisfied by a client that gave
/// up on any refusal at all, which is the behaviour
/// `a_refused_event_does_not_stop_the_drain` exists to forbid. `400` and
/// `404` are the two an operator actually meets — a body OpenPanel will not
/// take, and a `/track` path typed wrong — and neither is a reason to
/// abandon the events queued behind it.
#[tokio::test]
async fn a_per_event_refusal_still_does_not_stop_the_drain() {
    for refusal in [
        axum::http::StatusCode::BAD_REQUEST,
        axum::http::StatusCode::NOT_FOUND,
    ] {
        let collector = spawn_collector_with(Duration::ZERO, usize::MAX, refusal).await;
        let env = env(&collector.url, &[(DEPLOYMENT_ENV, "hosted-tenant")]);
        let tracker = build(&resolve(Deployment::from_env(&env), &env), envelope());

        for _ in 0..3 {
            tracker.track(Event::InstanceStarted {
                companies: 1,
                storage: "fs",
                setup_complete: true,
            });
        }
        tracker.flush().await;

        assert_eq!(
            collector.hits.load(Ordering::SeqCst),
            3,
            "{refusal} is about one event, so the two behind it must still be tried"
        );
        collector.stop().await;
    }
}

/// **A drain cancelled mid-flight says how many events it lost.**
///
/// This is the shutdown budget, reproduced. `src/bin/opencompany.rs` wraps
/// the final flush in a `tokio::time::timeout` of at most two seconds
/// (`server::shutdown::flush_budget`), and OpenPanel has no batch endpoint,
/// so a queue of `n` costs `n` sequential round trips. When the budget runs
/// out the future is **dropped mid-drain**: the events already taken off the
/// queue are gone, and before `CancelledDrain` nothing in this module said
/// so — the only trace was a `debug!` at the call site naming no count.
///
/// The loss is not fixed, deliberately (see `Inner::drain` for why bounded
/// concurrency is the wrong trade against the black-hole guarantee). What
/// is fixed is the silence, so this asserts the **count**, which is the part
/// an operator can act on. Asserted on the counter rather than on a log
/// line, because a test that needs a subscriber to see a regression is a
/// test that stops seeing it the day the subscriber changes.
///
/// A 300 ms collector and a 120 ms budget: the first event is still in
/// flight when the timeout fires, so four of five are certain to be lost —
/// no timing race, because the assertion is a lower bound rather than an
/// exact count.
#[tokio::test]
async fn a_cancelled_drain_reports_the_tail_it_lost() {
    let collector =
        spawn_collector_with(Duration::from_millis(300), 0, axum::http::StatusCode::OK).await;
    // Built directly rather than through `build`, because the counter is on
    // the concrete type and `build` hands back an `Arc<dyn Tracker>`.
    let tracker = HttpOpenPanelTracker::new(
        &collector.url,
        &crate::analytics::config::ClientCredentials::new(TEST_CLIENT_ID),
        envelope(),
    )
    .expect("the client builds");

    for _ in 0..5 {
        tracker.track(Event::InstanceStarted {
            companies: 1,
            storage: "fs",
            setup_complete: true,
        });
    }

    assert_eq!(
        tracker.lost_to_cancellation(),
        0,
        "nothing is lost before a drain is cancelled, or the assertion below is \
         measuring the wrong thing"
    );

    // Stands in for the shutdown budget, an order of magnitude smaller so
    // the test does not take two seconds to prove a two-second bound.
    let outcome = tokio::time::timeout(Duration::from_millis(120), tracker.flush()).await;
    assert!(
        outcome.is_err(),
        "the flush finished inside the budget, so nothing was cancelled and this \
         test proves nothing"
    );

    assert!(
        tracker.lost_to_cancellation() >= 4,
        "a cancelled drain lost {} events and reported {} — the tail of a shutdown \
         flush must be counted, not dropped in silence",
        5 - collector.hits.load(Ordering::SeqCst),
        tracker.lost_to_cancellation()
    );
    collector.stop().await;
}

/// **A loopback endpoint does not go through the system proxy.**
///
/// The loopback exception in `config::is_secure_endpoint` rests entirely on
/// the claim that such a request does not leave the host. A system proxy
/// makes that false: `reqwest`'s builder defaults to `auto_sys_proxy: true`,
/// which reads `HTTP_PROXY`/`ALL_PROXY` and takes exclusions **only** from
/// `NO_PROXY` — hyper-util 0.1.20's matcher has no implicit carve-out for
/// `localhost` or `127.0.0.0/8` (read, not assumed). So on a host with a
/// proxy configured, `http://127.0.0.1:…/track` went to the proxy in
/// cleartext with both credential headers on it, and the endpoint check
/// prevented nothing.
///
/// Two servers and one variable: a stand-in "proxy" that records anything
/// it is handed, and the real collector. With `HTTP_PROXY` pointing at the
/// first, the request must still arrive at the second. Asserting the proxy
/// saw **zero** is the security property; asserting the collector saw the
/// events is what stops that zero from being vacuous.
///
/// Mutates the process environment, so it holds the crate-wide
/// [`crate::test_support::EnvVarGuard`] — `reqwest` reads these variables
/// from the real environment at client-build time, which is the one thing
/// this crate's `MapEnv` seam cannot intercept.
#[tokio::test]
async fn a_loopback_endpoint_never_goes_through_a_system_proxy() {
    // Stands in for a corporate proxy: records every request and would be
    // the thing receiving the credential if the client honoured it.
    let proxy = spawn_collector().await;
    let collector = spawn_collector().await;

    let tracker = {
        let env = crate::test_support::EnvVarGuard::capture(&[
            "HTTP_PROXY",
            "http_proxy",
            "ALL_PROXY",
            "all_proxy",
            "NO_PROXY",
            "no_proxy",
        ]);
        // A proxy for everything, and no exclusions at all — the shape that
        // used to divert this traffic.
        env.remove("NO_PROXY");
        env.remove("no_proxy");
        env.remove("http_proxy");
        env.remove("all_proxy");
        env.set("ALL_PROXY", proxy.url.trim_end_matches("/track"));
        env.set("HTTP_PROXY", proxy.url.trim_end_matches("/track"));
        // Built inside the guard: `reqwest` samples the environment here,
        // not at send time.
        HttpOpenPanelTracker::new(
            &collector.url,
            &crate::analytics::config::ClientCredentials::new(TEST_CLIENT_ID),
            envelope(),
        )
        .expect("the client builds")
    };

    for _ in 0..2 {
        tracker.track(Event::InstanceStarted {
            companies: 1,
            storage: "fs",
            setup_complete: true,
        });
    }
    tracker.flush().await;

    assert_eq!(
        proxy.hits.load(Ordering::SeqCst),
        0,
        "a loopback endpoint went through the system proxy, so the client id \
         left the host in cleartext and the loopback exception protects nothing"
    );
    assert_eq!(
        collector.hits.load(Ordering::SeqCst),
        2,
        "and the events must still reach the collector directly, or the zero above \
         is a client that simply sent nothing"
    );
    proxy.stop().await;
    collector.stop().await;
}

/// **The transport refuses to be built for an endpoint the credential
/// cannot safely cross**, even from inside the crate.
///
/// `HttpOpenPanelTracker::new` is `pub(crate)` so that
/// [`build`] — which takes a `&Decision`, and a `Decision::Report` is what
/// `resolve` produces — is the only way to obtain a tracker. That closes the
/// route from outside. This closes the route from *inside*: a future caller
/// in this crate that reaches past `resolve` with
/// `http://collector.internal/track` would otherwise get a tracker that
/// posts the client id across a network in cleartext, with
/// `is_cleartext` politely turning off the proxy on the way.
///
/// The assertion calls `config::is_secure_endpoint` rather than restating
/// the rule, so there is one implementation of it and no second reader to
/// drift.
#[tokio::test]
#[should_panic(expected = "the credential cannot safely cross")]
async fn the_transport_refuses_an_endpoint_that_never_passed_resolve() {
    let _ = HttpOpenPanelTracker::new(
        "http://collector.internal/track",
        &crate::analytics::config::ClientCredentials::new(TEST_CLIENT_ID),
        envelope(),
    );
}

/// The control: the same construction with a loopback endpoint must be
/// accepted, or the test above would pass for a constructor that refused
/// everything.
#[tokio::test]
async fn the_transport_accepts_an_endpoint_resolve_would_have_allowed() {
    for allowed in [
        "http://127.0.0.1:9/track",
        "https://collector.invalid/track",
    ] {
        assert!(
            HttpOpenPanelTracker::new(
                allowed,
                &crate::analytics::config::ClientCredentials::new(TEST_CLIENT_ID),
                envelope(),
            )
            .is_ok(),
            "{allowed} is one resolve would allow and must still build"
        );
    }
}

/// The control: a drain that **finishes** must report nothing lost.
///
/// Without it, `lost_to_cancellation() >= 4` above would also pass for a
/// guard that fired on every drain, which would turn a real signal into a
/// line an operator learns to ignore — and this module's whole problem is
/// notices nobody reads.
#[tokio::test]
async fn a_drain_that_finishes_reports_nothing_lost() {
    let collector = spawn_collector().await;
    let tracker = HttpOpenPanelTracker::new(
        &collector.url,
        &crate::analytics::config::ClientCredentials::new(TEST_CLIENT_ID),
        envelope(),
    )
    .expect("the client builds");

    for _ in 0..3 {
        tracker.track(Event::InstanceStarted {
            companies: 1,
            storage: "fs",
            setup_complete: true,
        });
    }
    tracker.flush().await;

    assert_eq!(
        collector.hits.load(Ordering::SeqCst),
        3,
        "the positive control: all three really did land"
    );
    assert_eq!(
        tracker.lost_to_cancellation(),
        0,
        "a drain that ran to completion must not report a lost tail"
    );
    collector.stop().await;
}

/// **The end-to-end check against a real OpenPanel instance.**
///
/// `#[ignore]` because it needs a collector, a credential and a network,
/// none of which CI has. Everything above proves this transport does what
/// this repository believes OpenPanel wants; only this proves OpenPanel
/// agrees. Every failure mode in this module is silent, so "the unit suite
/// is green" and "events are landing" are genuinely different claims.
///
/// ```text
/// OPENCOMPANY_ANALYTICS_ENDPOINT=https://<host>/api/track \
/// OPENCOMPANY_ANALYTICS_CLIENT_ID=<uuid> \
///   cargo test --features analytics -- --ignored --nocapture \
///   analytics::openpanel::test::a_real_collector_accepts_an_event
/// ```
///
/// No client secret: the collector's clients run with "ignore CORS and
/// secret". The client id comes from the environment and is never written
/// anywhere: not to a fixture, not to a log line, and not to this test's output,
/// which prints only the `profileId` it sent and the ids the collector
/// returned — enough to find the event in the dashboard and nothing more.
///
/// It asserts a `2xx` **and** that the body names a `deviceId`, because a
/// collector fronted by a proxy that swallows the request can answer `200`
/// with something else entirely, and a status-only assertion would call
/// that a pass.
#[tokio::test]
#[ignore = "needs a real OpenPanel instance and a credential from the environment"]
async fn a_real_collector_accepts_an_event() {
    use crate::app::config::EnvSource;

    let os_env = crate::app::config::ProcessEnv;
    let endpoint = os_env
        .get(ENDPOINT_ENV)
        .expect("set OPENCOMPANY_ANALYTICS_ENDPOINT");
    let credentials = match resolve(
        Deployment::HostedTenant,
        &MapEnv::new([
            (
                CLIENT_ID_ENV,
                os_env.get(CLIENT_ID_ENV).expect("set the client id"),
            ),
            (ENDPOINT_ENV, endpoint.clone()),
        ]),
    ) {
        crate::analytics::config::Decision::Report { credentials, .. } => credentials,
        other => panic!("the environment does not resolve to reporting: {other:?}"),
    };

    // A run-specific id, so the event is findable and no real instance's
    // numbers are disturbed.
    let id = OpaqueId::instance(&format!("{:032x}", crate::ports::now_millis()));
    println!("posting as profileId {}", id.as_str());

    let body = crate::analytics::payload(
        &Envelope::new(id, Deployment::HostedTenant, Cognition::default()),
        &Event::InstanceStarted {
            companies: 1,
            storage: "fs",
            setup_complete: true,
        },
    );

    let response = reqwest::Client::builder()
        .default_headers(super::http::request_headers(&credentials))
        .build()
        .expect("a client")
        .post(&endpoint)
        .json(&body)
        .send()
        .await
        .expect("the collector is reachable");

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    assert!(
        status.is_success(),
        "the collector refused the event with {status}: {text}"
    );
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    assert!(
        parsed.get("deviceId").is_some(),
        "a 2xx with no deviceId is a proxy answering, not OpenPanel accepting: \
         {status} {text}"
    );
    println!("collector accepted it: {status} {text}");
}

/// **The header-safety check in `config` really is a subset of what a
/// header value accepts.**
///
/// `config::resolve` refuses a credential it judges unfit for a header, and
/// it makes that judgement in the un-gated build, where `reqwest` may not
/// even be in the dependency graph — so the rule is written by hand there
/// and is deliberately *stricter* than `HeaderValue`. That is only safe
/// while the subset claim holds: anything `resolve` accepts, the transport
/// must be able to put on the wire. Nothing else in the tree would notice
/// the day it stopped holding, so it is asserted here, in the one lane that
/// has a `HeaderValue` to compare against.
///
/// The reverse containment is deliberately **not** asserted: `HeaderValue`
/// takes space, tab and the whole `0xA0..=0xFF` range, and refusing those is
/// the point.
#[test]
fn the_header_safety_check_is_a_subset_of_what_a_header_accepts() {
    use crate::analytics::config::Decision;
    use reqwest::header::HeaderValue;

    // Every single byte, plus the multi-byte shapes a mangled client id
    // actually arrives in.
    let mut candidates: Vec<String> = (1u8..=255)
        .map(|byte| format!("ok{}", byte as char))
        .collect();
    candidates.extend(
        [
            "not-a-real-client-id",
            "op_sk_9zQx-4Kd_7Yb2Lp0",
            "550e8400-e29b-41d4-a716-446655440000",
            "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXo=",
            "wrapped\nclient-id",
            "tab\tseparated",
            "spaced out",
            "caf\u{e9}-latte",
            "\u{4f8b}\u{3048}",
        ]
        .map(str::to_string),
    );

    let mut accepted = 0usize;
    for candidate in &candidates {
        let decision = resolve(
            Deployment::HostedTenant,
            &env(
                "https://collector.invalid/track",
                &[(CLIENT_ID_ENV, candidate.as_str())],
            ),
        );
        if matches!(decision, Decision::Report { .. }) {
            accepted += 1;
            assert!(
                HeaderValue::from_str(candidate.trim()).is_ok(),
                "`resolve` accepted {candidate:?}, which cannot go in a header — the \
                 subset claim in `config::is_header_safe` no longer holds"
            );
        }
    }

    // The control: the loop above would pass trivially if `resolve` had
    // started refusing everything.
    assert!(
        accepted > 50,
        "only {accepted} of {} candidates were accepted; the check has become so \
         strict that the subset assertion means nothing",
        candidates.len()
    );
}
