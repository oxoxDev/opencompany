//! Transport-decision tests (silence/consent/redaction/credential
//! handling), split out of `openpanel_tests.rs` (topic split, >750 lines).

use super::*;
use crate::analytics::config::{CLIENT_ID_ENV, ENABLE_ENV, ENDPOINT_ENV, resolve};
use crate::analytics::types::OpaqueId;
use crate::analytics::{Event, Outcome, Trigger};
use crate::app::config::MapEnv;
use crate::app::deployment::{DEPLOYMENT_ENV, Deployment};
use crate::ports::brain::Cognition;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// An obviously-fake client id. Never a real one, in a file or anywhere else.
const TEST_CLIENT_ID: &str = "not-a-real-client-id";

/// The headers each request arrived with, in order, name and value.
type SeenHeaders = Arc<std::sync::Mutex<Vec<Vec<(String, String)>>>>;

/// A local collector that counts what it is sent and keeps the bodies and
/// the headers.
struct Collector {
    hits: Arc<AtomicUsize>,
    bodies: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    headers: SeenHeaders,
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
    let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
    let headers = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_hits = hits.clone();
    let seen_bodies = bodies.clone();
    let seen_headers = headers.clone();

    let app = axum::Router::new().route(
        "/track",
        axum::routing::post(
            move |received: axum::http::HeaderMap,
                  axum::Json(body): axum::Json<serde_json::Value>| {
                let hits = seen_hits.clone();
                let bodies = seen_bodies.clone();
                let headers = seen_headers.clone();
                async move {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    let seen = hits.fetch_add(1, Ordering::SeqCst);
                    bodies.lock().unwrap().push(body);
                    headers.lock().unwrap().push(
                        received
                            .iter()
                            .map(|(name, value)| {
                                (
                                    name.as_str().to_string(),
                                    value.to_str().unwrap_or_default().to_string(),
                                )
                            })
                            .collect(),
                    );
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
        bodies,
        headers,
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

    fn header(&self, request: usize, name: &str) -> Option<String> {
        self.headers.lock().unwrap()[request]
            .iter()
            .find(|(seen, _)| seen == name)
            .map(|(_, value)| value.clone())
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

fn events() -> Vec<Event> {
    vec![
        Event::InstanceStarted {
            companies: 1,
            storage: "fs",
            setup_complete: true,
        },
        Event::TurnFinished {
            trigger: Trigger::OperatorMessage,
            outcome: Outcome::Ok,
            failure: None,
            duration_ms: 12,
            effects_executed: 0,
            approvals_parked: 0,
        },
    ]
}

/// **Issue #1739's first acceptance criterion.** A build that *has* the
/// transport compiled in, pointed at a live collector, with a credential in
/// the environment, and not declared hosted: it must send nothing.
///
/// Note what is deliberately stacked against the assertion — the feature is
/// on, the client exists, the endpoint resolves, the client id is
/// present. The only thing that is not is consent. That is
/// the configuration a self-hoster who copied a hosted deployment's env file
/// would have.
#[tokio::test]
async fn a_self_hosted_build_makes_no_request() {
    let collector = spawn_collector().await;
    let env = env(&collector.url, &[]);

    let decision = resolve(Deployment::from_env(&env), &env);
    let tracker = build(&decision, envelope());
    for event in events() {
        tracker.track(event);
    }
    tracker.flush().await;

    assert_eq!(
        collector.hits.load(Ordering::SeqCst),
        0,
        "a self-hosted build must not dial out"
    );
    collector.stop().await;
}

/// The positive control that makes the test above non-vacuous: the same
/// collector, the same events, the same code path, one variable changed.
///
/// It also pins the whole wire contract — **one request per event**, the
/// client-id header by its exact spelling and **no** client-secret header,
/// and OpenPanel's discriminated-union body with the identity as `profileId` rather than as a property.
#[tokio::test]
async fn a_hosted_tenant_reports_with_the_full_envelope() {
    let collector = spawn_collector().await;
    let env = env(&collector.url, &[(DEPLOYMENT_ENV, "hosted-tenant")]);

    let decision = resolve(Deployment::from_env(&env), &env);
    assert!(decision.reports(), "{decision:?}");
    let tracker = build(&decision, envelope());
    for event in events() {
        tracker.track(event);
    }
    tracker.flush().await;

    // Two events, two requests. OpenPanel has no batch endpoint, so this is
    // the one number that changed shape rather than value.
    assert_eq!(
        collector.hits.load(Ordering::SeqCst),
        2,
        "one request per event"
    );

    let bodies = collector.bodies.lock().unwrap().clone();
    let first = &bodies[0];
    assert_eq!(first["type"], "track");
    assert_eq!(first["payload"]["name"], "instance_started");
    assert_eq!(
        first["payload"]["profileId"],
        "i_0123456789abcdef0123456789abcdef"
    );

    let properties = &first["payload"]["properties"];
    assert_eq!(properties["deployment"], "hosted-tenant");
    assert!(properties["app_version"].is_string());
    assert!(properties["harness_in_build"].is_boolean());
    assert_eq!(bodies[1]["payload"]["name"], "turn_finished");

    for request in 0..2 {
        assert_eq!(
            collector.header(request, CLIENT_ID_HEADER).as_deref(),
            Some(TEST_CLIENT_ID),
            "request {request} carried no client id header"
        );
        assert_eq!(
            collector
                .header(request, "openpanel-client-secret")
                .as_deref(),
            None,
            "request {request} carried a client secret header; the collector's \
             clients run with the secret check off and none is configured"
        );
    }

    collector.stop().await;
}

/// **The credential travels in a header and nowhere else.**
///
/// The transport this replaced stamped Mixpanel's token into every event's
/// property bag, which put a credential one `dbg!` away from a test fixture
/// or a captured body. Nothing does that now, and this is the assertion that
/// keeps it true: not one byte of the client id appears in any body on the
/// wire.
#[tokio::test]
async fn no_credential_reaches_the_request_body() {
    let collector = spawn_collector().await;
    let env = env(&collector.url, &[(DEPLOYMENT_ENV, "hosted-tenant")]);
    let tracker = build(&resolve(Deployment::from_env(&env), &env), envelope());
    for event in events() {
        tracker.track(event);
    }
    tracker.flush().await;

    for body in collector.bodies.lock().unwrap().iter() {
        let rendered = body.to_string().to_ascii_lowercase();
        assert!(
            !rendered.contains(&TEST_CLIENT_ID.to_ascii_lowercase()),
            "the body carried the client id: {rendered}"
        );
    }
    // The self-check: the needle really is findable where it *is* supposed
    // to be, or the guard above would pass on a transport that sent no
    // credential at all.
    assert_eq!(
        collector.header(0, CLIENT_ID_HEADER).as_deref(),
        Some(TEST_CLIENT_ID)
    );

    collector.stop().await;
}

/// An operator who switched it off stays off, even on a hosted tenant.
#[tokio::test]
async fn an_opted_out_tenant_makes_no_request() {
    let collector = spawn_collector().await;
    let env = env(
        &collector.url,
        &[(DEPLOYMENT_ENV, "hosted-tenant"), (ENABLE_ENV, "off")],
    );

    let tracker = build(&resolve(Deployment::from_env(&env), &env), envelope());
    for event in events() {
        tracker.track(event);
    }
    tracker.flush().await;

    assert_eq!(collector.hits.load(Ordering::SeqCst), 0);
    collector.stop().await;
}

/// **A refused event does not stop the drain.**
///
/// A per-event HTTP status is a per-event answer — a name the collector
/// rejects, a body it will not take — and the events behind it may be
/// perfectly good. Without this, one malformed event would silence an entire
/// drain, which is the failure mode that matters most in a module where
/// every other failure is already silent.
#[tokio::test]
async fn a_refused_event_does_not_stop_the_drain() {
    let collector =
        spawn_collector_with(Duration::ZERO, 1, axum::http::StatusCode::BAD_REQUEST).await;
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
        "the two events behind a refused one must still be attempted"
    );
    collector.stop().await;
}

/// **A refused *credential* does stop the drain, unlike a refused event.**
///
/// A `401` is not the collector's verdict on one event; it is its verdict on
/// this process, so every event behind it in the queue gets the same answer.
/// Carrying on would fire up to 500 requests every thirty seconds for the
/// life of a misconfigured tenant — a thousand a minute at the operator's
/// own collector — to learn something already known.
///
/// The contrast with `a_refused_event_does_not_stop_the_drain` is the point:
/// same collector, same three events, one status code changed, opposite
/// behaviour. Neither test means much without the other.
#[tokio::test]
async fn a_refused_credential_stops_the_drain() {
    let collector = spawn_collector_with(
        Duration::ZERO,
        usize::MAX,
        axum::http::StatusCode::UNAUTHORIZED,
    )
    .await;
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
        "a 401 is about the credential, not about the event, so the two behind it \
         must not be attempted"
    );
    collector.stop().await;
}

/// **The write credential never follows a redirect to another host.**
///
/// The leak this closes is not exotic. `reqwest`'s default policy follows
/// ten hops, and its cross-origin sanitization
/// (`redirect.rs::remove_sensitive_headers`, 0.12.28) removes exactly
/// `Authorization`, `Cookie`, `cookie2`, `Proxy-Authorization` and
/// `WWW-Authenticate` — and nothing else. `openpanel-client-id` is none
/// of them, so before [`reqwest::redirect::Policy::none`] a single `307`
/// from the configured collector handed this instance's long-lived write
/// credential to whatever host the `Location` named.
///
/// `set_sensitive` is not a defence and is worth naming, because it looks
/// like one in the source: it governs `Debug` output and HPACK indexing,
/// and has no bearing on which headers survive a hop.
///
/// Two collectors on two ports, so `next.port_or_known_default() !=
/// previous.port_or_known_default()` — reqwest's own cross-host test — is
/// unambiguously true and the sanitization it does perform is in play. The
/// assertion is on the **destination**: it must be untouched. Asserting
/// only "the redirect was not followed" would pass against a client that
/// followed it and merely dropped the header, which is a different and
/// weaker property than the one being claimed.
#[tokio::test]
async fn a_redirect_never_carries_the_credential_to_another_host() {
    // Where a followed redirect would land: a real collector that records
    // every header of everything it is sent.
    let elsewhere = spawn_collector().await;
    let target = elsewhere.url.clone();

    // The configured endpoint: answers every POST with a 307 to the other
    // collector, on a different port and so a different origin.
    let redirected = Arc::new(AtomicUsize::new(0));
    let counted = redirected.clone();
    let app = axum::Router::new().route(
        "/track",
        axum::routing::post(move || {
            let hits = counted.clone();
            let target = target.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    axum::http::StatusCode::TEMPORARY_REDIRECT,
                    [(axum::http::header::LOCATION, target)],
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/track", listener.local_addr().unwrap());
    let (shutdown, rx) = tokio::sync::oneshot::channel();
    let redirector = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
    });

    let env = env(&url, &[(DEPLOYMENT_ENV, "hosted-tenant")]);
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
        elsewhere.hits.load(Ordering::SeqCst),
        0,
        "a redirect must not carry the client credential to a host \
         OPENCOMPANY_ANALYTICS_ENDPOINT never named"
    );
    assert_eq!(
        redirected.load(Ordering::SeqCst),
        1,
        "a redirecting endpoint is a verdict on the endpoint, not on one event, so \
         the two behind it must not be attempted"
    );

    let _ = shutdown.send(());
    let _ = redirector.await;
    elsewhere.stop().await;
}

/// The control that makes the test above non-vacuous.
///
/// `elsewhere.hits == 0` would also hold if the destination collector were
/// simply broken, or if `spawn_collector` did not record what it received.
/// Same collector, same events, pointed at directly rather than through a
/// redirect: it must see all three requests, carrying the client id, so the
/// zero above is about the redirect and nothing else.
#[tokio::test]
async fn the_redirect_destination_would_have_recorded_the_credential() {
    let elsewhere = spawn_collector().await;
    let env = env(&elsewhere.url, &[(DEPLOYMENT_ENV, "hosted-tenant")]);
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
        elsewhere.hits.load(Ordering::SeqCst),
        3,
        "the destination records what it is sent, so the zero above is the redirect \
         policy rather than a collector that counts nothing"
    );
    assert_eq!(
        elsewhere.header(0, CLIENT_ID_HEADER).as_deref(),
        Some(TEST_CLIENT_ID),
        "and it records the credential header, which is the thing that must not \
         have arrived across a redirect"
    );
    elsewhere.stop().await;
}

/// The queue is bounded. An unreachable collector must cost telemetry, not
/// a tenant container's memory — and `track` must not block whatever the
/// collector does.
#[tokio::test]
async fn the_queue_is_bounded() {
    // Nothing listens here; the point is that `track` never blocks and
    // never grows without bound whatever the collector does.
    let env = env(
        "http://127.0.0.1:1/track",
        &[(DEPLOYMENT_ENV, "hosted-tenant")],
    );
    let tracker = build(&resolve(Deployment::from_env(&env), &env), envelope());
    for _ in 0..2_000 {
        tracker.track(Event::InstanceStarted {
            companies: 1,
            storage: "fs",
            setup_complete: true,
        });
    }
    // No assertion on an internal count — the observable property is that
    // this returns at all, promptly, with no reachable collector.
}

/// **An unreachable collector costs one timeout, not one per queued event.**
///
/// This is the case losing the batch endpoint created. With a single POST
/// carrying everything, an unreachable collector cost exactly one
/// `SEND_TIMEOUT`. One request per event, drained sequentially, would cost
/// `queued × SEND_TIMEOUT` — up to forty minutes at a full queue — during
/// which the shutdown flush is blocked behind the same lock and a
/// container's `SIGTERM` budget is long gone.
///
/// Asserted on **connections the collector actually accepted**, not on
/// elapsed time, because a timing threshold on a black-holing socket is a
/// flaky test. The listener accepts and never answers, so each attempt is a
/// real connection that pays the full timeout: three queued events must
/// produce **one** connection, not three.
#[tokio::test]
async fn an_unreachable_collector_costs_one_timeout_for_the_whole_drain() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/track", listener.local_addr().unwrap());
    let counted = accepted.clone();
    // Accepts and holds. Never reads, never answers — the shape a collector
    // behind a wedged proxy has, and the one a refused port does not
    // exercise because it fails instantly.
    let black_hole = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            held.push(socket);
        }
    });

    let env = env(&url, &[(DEPLOYMENT_ENV, "hosted-tenant")]);
    let tracker = build(&resolve(Deployment::from_env(&env), &env), envelope());
    for _ in 0..3 {
        tracker.track(Event::InstanceStarted {
            companies: 1,
            storage: "fs",
            setup_complete: true,
        });
    }

    let started = Instant::now();
    tracker.flush().await;
    let waited = started.elapsed();

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "the drain must give up after the first transport failure, not pay a \
         timeout for every queued event"
    );
    assert!(
        waited < Duration::from_secs(12),
        "a drain against a black hole took {waited:?}, which is more than one \
         send timeout and would outlive a container's shutdown budget"
    );
    // And the flush still returned, which is the property `Tracker::flush`
    // promises: analytics never prevents a shutdown.
    black_hole.abort();
}

/// **A transport failure must not carry the collector credential.**
///
/// `OPENCOMPANY_ANALYTICS_ENDPOINT` names a collector the operator runs, and
/// such a collector is routinely fronted by an authenticated proxy, which
/// carries its key in one of the two places a URL can hold one.
/// `reqwest::Error` retains the request URL and prints it, so an unreachable
/// collector — a routine event, not an exotic one — wrote that key into the
/// debug log.
///
/// Measured against reqwest 0.12.28 rather than assumed, and the two places
/// do **not** behave alike:
///
/// | in the endpoint | what `reqwest::Error`'s `Display` printed |
/// |---|---|
/// | `http://someone:KEY@127.0.0.1:1/track` | `… for url (http://127.0.0.1:1/track)` — userinfo already stripped |
/// | `http://127.0.0.1:1/track?key=KEY` | `… for url (http://127.0.0.1:1/track?key=KEY)` — **leaked verbatim** |
///
/// So the query string is the live leak; userinfo is not, today. Both are
/// covered here anyway, because "the dependency strips it" is not a
/// property this crate owns — it is one `cargo update` from being false,
/// and nothing here would fail when it changed. `without_url` removes the
/// URL outright, so neither shape can reach the line whatever reqwest
/// decides to print.
///
/// Asserted **case-insensitively**, with the self-check below: this guard
/// once shipped in a form that passed a deliberate leak, because the value
/// came back lowercased.
#[tokio::test]
async fn a_transport_failure_never_carries_the_endpoint_credential() {
    const SECRET: &str = "NotARealCollectorKey";
    let needle = SECRET.to_ascii_lowercase();

    // The self-check, on the shape that is measurably still leaking. A
    // guard that cannot find the needle in the **unstripped** error proves
    // nothing about the stripped one — the needle may never have been
    // there at all. If reqwest ever starts redacting query strings too,
    // this fails loudly and says so, rather than leaving a guard behind
    // that asserts nothing.
    let leaky = format!("http://127.0.0.1:1/track?key={SECRET}");
    let unstripped = send_failing(&leaky).await.to_string();
    assert!(
        unstripped.to_ascii_lowercase().contains(&needle),
        "the needle must be findable before stripping, or this guard is \
         vacuous: {unstripped}"
    );

    for endpoint in [
        // Port 1 refuses, so each of these is a real transport error rather
        // than a fabricated one.
        format!("http://someone:{SECRET}@127.0.0.1:1/track"),
        leaky.clone(),
        format!("http://someone:{SECRET}@127.0.0.1:1/track?key={SECRET}"),
    ] {
        let logged = super::http::loggable_send_error(send_failing(&endpoint).await);
        assert!(
            !logged.to_ascii_lowercase().contains(&needle),
            "the transport error leaked the collector credential from \
             {endpoint:?}: {logged}"
        );
        assert!(
            !logged.is_empty(),
            "stripping the URL must still leave the operator a reason: {logged}"
        );

        // And the destination is still named on the same line — through the
        // one redaction helper the boot line uses, not a second one.
        let named = crate::analytics::boot::loggable_endpoint(&endpoint);
        assert!(
            !named.to_ascii_lowercase().contains(&needle),
            "the endpoint field leaked it instead: {named}"
        );
        assert!(
            named.contains("127.0.0.1"),
            "the operator still has to be able to tell where it was going: {named}"
        );
    }
}

/// One real, refused request. Nothing listens on port 1.
async fn send_failing(endpoint: &str) -> reqwest::Error {
    reqwest::Client::new()
        .post(endpoint)
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect_err("nothing listens on port 1")
}
