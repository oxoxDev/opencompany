//! Endpoint validation and reporting tests, split out of
//! `config_tests.rs` (topic split, >750 lines). `configured()` and
//! `TEST_ENDPOINT` are duplicated from `config_credential_tests.rs`.

use super::*;
use crate::app::config::MapEnv;

/// A collector address that resolves nowhere. Every reporting test needs
/// one now: there is no default endpoint left to fall back to.
const TEST_ENDPOINT: &str = "https://collector.invalid/track";

/// A fully configured reporting environment, which `pairs` then overrides.
///
/// It takes two variables, and that is the whole tenant contract: an
/// OpenPanel deployment configures a client id and the address of the
/// collector it self-hosts. There is no client secret.
fn configured(pairs: &[(&str, &str)]) -> MapEnv {
    let mut all = vec![
        (CLIENT_ID_ENV, "not-a-real-client-id"),
        (ENDPOINT_ENV, TEST_ENDPOINT),
    ];
    all.extend_from_slice(pairs);
    MapEnv::new(all)
}

/// The positive control for the endpoint group, and deliberately
/// **insensitive** to the trim: no surrounding whitespace, so this test
/// passes both with the filter and without it. Without such a control,
/// "every test in the group fails when I revert the fix" would be evidence
/// that the group asserts the implementation rather than the behaviour.
#[test]
fn a_configured_endpoint_is_reported_to_exactly() {
    match resolve(
        Deployment::HostedTenant,
        &configured(&[(ENDPOINT_ENV, "http://127.0.0.1:9/track")]),
    ) {
        Decision::Report { endpoint, .. } => assert_eq!(endpoint, "http://127.0.0.1:9/track"),
        other => panic!("{other:?}"),
    }
}

/// **A malformed endpoint is silence with a reason, not reporting.**
///
/// `collector.internal/track` — a hostname written without a scheme, which
/// is how anyone would first write one — used to resolve to
/// `Decision::Report`. Boot printed "reporting to collector.internal/track",
/// the tracker was installed, and every send died inside `reqwest` behind a
/// `debug!` line no operator has enabled. The product said something
/// true-sounding and then did nothing, which is the one failure this module
/// exists to make impossible — and it matters more now that every reporting
/// deployment types this variable by hand.
#[test]
fn a_malformed_endpoint_is_silence_rather_than_a_broken_report() {
    for unusable in [
        "collector.internal/track",
        "collector.internal",
        "/track",
        "://collector.internal/track",
        "ftp://collector.internal/track",
        "file:///tmp/track",
        "https://",
        "http://someone:hunter2@/track",
        "http://collector internal/track",
    ] {
        let decision = resolve(
            Deployment::HostedTenant,
            &configured(&[(ENDPOINT_ENV, unusable)]),
        );
        assert_eq!(
            decision,
            Decision::Silent(Silence::UnusableEndpoint),
            "{unusable:?} must not resolve to a report that cannot be sent"
        );
        assert!(!decision.reports(), "{unusable:?}");
    }
}

/// The reason names the variable and **never the value**: a collector
/// fronted by an authenticated proxy carries its key in the very URL that
/// was rejected, so quoting the bad value would put a credential in the boot
/// line of every misconfigured tenant. Asserted case-insensitively, because
/// a guard that matched exact case would read a lowercased leak as clean.
#[test]
fn the_unusable_endpoint_reason_never_quotes_the_endpoint() {
    const SECRET: &str = "NotARealCollectorKey";
    let reason = Silence::UnusableEndpoint.as_str();
    assert!(
        reason.contains("OPENCOMPANY_ANALYTICS_ENDPOINT"),
        "the reason must name the variable to act on: {reason}"
    );

    // Rejected for having no scheme, and carrying a credential while it is
    // rejected — which is exactly the case that would leak.
    let raw = format!("collector.internal/track?key={SECRET}");
    assert_eq!(
        resolve(
            Deployment::HostedTenant,
            &configured(&[(ENDPOINT_ENV, raw.as_str())])
        ),
        Decision::Silent(Silence::UnusableEndpoint)
    );
    let printed = format!("{:?} {}", Silence::UnusableEndpoint, reason);
    assert!(
        !printed
            .to_ascii_lowercase()
            .contains(&SECRET.to_ascii_lowercase()),
        "the reason leaked the endpoint credential: {printed}"
    );
    // The self-check: the needle really is findable in the unredacted
    // value, in whatever case it comes back, or the guard above is vacuous.
    assert!(
        raw.to_ascii_lowercase()
            .contains(&SECRET.to_ascii_lowercase())
            && raw
                .to_ascii_uppercase()
                .to_ascii_lowercase()
                .contains(&SECRET.to_ascii_lowercase()),
        "the needle must be findable before redaction: {raw}"
    );
}

/// **A non-Unicode endpoint is unusable, not absent.**
///
/// It reads through `get_os` rather than `get` so that the two stay
/// distinguishable. `get` maps unreadable bytes to `None`, which would tell
/// an operator who mistyped their collector address that they had never set
/// one — sending them to add a variable that is already there.
///
/// Under the endpoint default this replaced, the same confusion was
/// materially worse: unreadable bytes fell back to `api.mixpanel.com`, so a
/// tenant that pointed analytics at its own collector and mistyped it
/// reported to a **third party** instead. There is no default left for it
/// to fall into, so this is now a diagnostic distinction rather than a
/// containment one — but it is the same read, kept for the same reason.
#[cfg(unix)]
#[test]
fn a_non_unicode_endpoint_is_unusable_rather_than_absent() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    struct NonUnicodeEndpoint;
    impl EnvSource for NonUnicodeEndpoint {
        fn get_os(&self, key: &str) -> Option<OsString> {
            match key {
                ENDPOINT_ENV => Some(OsString::from_vec(
                    [b"https://collector.invalid/".as_slice(), &[0xff, 0xfe]].concat(),
                )),
                CLIENT_ID_ENV => Some(OsString::from("not-a-real-client-id")),
                _ => None,
            }
        }
    }

    // The premise: a value `get` cannot see at all.
    assert_eq!(NonUnicodeEndpoint.get(ENDPOINT_ENV), None);
    assert!(NonUnicodeEndpoint.get_os(ENDPOINT_ENV).is_some());

    let decision = resolve(Deployment::HostedTenant, &NonUnicodeEndpoint);
    assert_eq!(decision, Decision::Silent(Silence::UnusableEndpoint));
    match &decision {
        Decision::Report { endpoint, .. } => {
            panic!("reported to {endpoint} — an endpoint the operator never configured")
        }
        Decision::Silent(_) => {}
    }
}

/// **The endpoint check agrees with what `reqwest` can actually send to.**
///
/// Every row was measured against reqwest 0.12.28 — `Url::parse`,
/// `Client::post(..).build()`, and for the scheme, what the send does — not
/// reasoned about. The rows marked below are the ones a hand-rolled grammar
/// check accepted and `reqwest` rejects; they resolved to `Decision::Report`
/// and then dropped every event, which is the very failure
/// `is_usable_endpoint` exists to prevent.
#[test]
fn the_endpoint_check_matches_what_the_transport_accepts() {
    // (endpoint, refusal) — `None` reports, `Some(reason)` is silence with
    // that reason. `UnusableEndpoint` means `reqwest` cannot send to it at
    // all; `InsecureEndpoint` means it could, and must not, because the
    // credential would be readable on the way.
    let measured: &[(&str, Option<Silence>)] = &[
        // Rejected by `Url::parse`. Each of these was accepted by the
        // hand-rolled check this replaced.
        ("http://[::1/track", Some(Silence::UnusableEndpoint)), // unclosed IPv6 bracket
        ("http://]::1[/track", Some(Silence::UnusableEndpoint)), // brackets inside out
        (
            "http://collector.internal:99999/track",
            Some(Silence::UnusableEndpoint),
        ), // port out of range
        (
            "http://collector.internal:65536/track",
            Some(Silence::UnusableEndpoint),
        ), // one past the top
        (
            "http://collector.internal:abc/track",
            Some(Silence::UnusableEndpoint),
        ), // port not a number
        (
            "http://host:8080:9090/track",
            Some(Silence::UnusableEndpoint),
        ), // two ports
        ("http://127.0.0.1.5/track", Some(Silence::UnusableEndpoint)), // IPv4-shaped, invalid
        (
            "http://999.999.999.999/track",
            Some(Silence::UnusableEndpoint),
        ), // IPv4-shaped, invalid
        // Rejected by `Url::parse` and by the hand-rolled check alike.
        ("collector.internal/track", Some(Silence::UnusableEndpoint)),
        ("collector.internal", Some(Silence::UnusableEndpoint)),
        ("/track", Some(Silence::UnusableEndpoint)),
        (
            "://collector.internal/track",
            Some(Silence::UnusableEndpoint),
        ),
        ("https://", Some(Silence::UnusableEndpoint)),
        (
            "http://someone:hunter2@/track",
            Some(Silence::UnusableEndpoint),
        ),
        (
            "http://collector internal/track",
            Some(Silence::UnusableEndpoint),
        ),
        // Parsed happily by `url` — and even built by `reqwest` — but not
        // sendable, so checked on top of the parse.
        (
            "ftp://collector.internal/track",
            Some(Silence::UnusableEndpoint),
        ), // scheme refused at send
        ("file:///tmp/track", Some(Silence::UnusableEndpoint)),
        // NOT here: `http:///track`. It looks like an empty host and is
        // not one — `url` normalizes it to `http://track/`, taking the
        // first path segment as the host, and `reqwest` sends to it. A
        // collector named `track` that does not resolve is an unreachable
        // collector like any other, which #1739 makes a no-op on purpose.
        //
        // Sendable, but plain `http` to a host that is not loopback: the
        // client id is a header on every request, so these would put it
        // on the wire in the clear. Each was accepted before the
        // `InsecureEndpoint` rule.
        (
            "http://collector.internal:65535/track",
            Some(Silence::InsecureEndpoint),
        ), // the top of the range
        (
            "http://collector.internal:/track",
            Some(Silence::InsecureEndpoint),
        ), // empty port is legal
        ("http://exa_mple.com/track", Some(Silence::InsecureEndpoint)),
        ("http://-example.com/track", Some(Silence::InsecureEndpoint)),
        (
            "http://\u{4f8b}\u{3048}.jp/track",
            Some(Silence::InsecureEndpoint),
        ),
        // A host that merely *starts* with a loopback address is not one.
        (
            "http://127.0.0.1.evil.example/track",
            Some(Silence::InsecureEndpoint),
        ),
        (
            "http://localhost.evil.example/track",
            Some(Silence::InsecureEndpoint),
        ),
        // Accepted, and the ones a deployment actually uses: `https`
        // anywhere, and `http` only to loopback.
        (TEST_ENDPOINT, None),
        ("http://127.0.0.1:9/track", None),
        ("http://127.0.0.1:9", None),
        ("http://localhost:9/track", None),
        ("http://LOCALHOST:9/track", None),
        ("https://collector.internal/track", None),
        ("HTTPS://collector.internal/track", None),
        (
            "https://collector.internal/track?key=NotARealCollectorKey",
            None,
        ),
        (
            "https://someone:NotARealCollectorKey@collector.internal/track",
            None,
        ),
        ("https://[::1]:8443/track", None),
        ("http://[::1]/track", None),
        ("https://collector.internal:8443/track#frag", None),
    ];

    for (endpoint, refusal) in measured {
        let decision = resolve(
            Deployment::HostedTenant,
            &configured(&[(ENDPOINT_ENV, endpoint)]),
        );
        match refusal {
            None => match decision {
                Decision::Report { endpoint: got, .. } => assert_eq!(&got, endpoint),
                other => panic!("{endpoint:?} must still report: {other:?}"),
            },
            Some(reason) => assert_eq!(
                decision,
                Decision::Silent(*reason),
                "{endpoint:?} must resolve to silence with {reason:?}"
            ),
        }
    }
}

/// **A plain `http` endpoint to a non-loopback host is silence, not a
/// credential in the clear.**
///
/// The OpenPanel client id — the whole write credential, with the
/// collector's secret check off — is a request header on *every* request, so
/// `OPENCOMPANY_ANALYTICS_ENDPOINT=http://collector.internal/track` writes a
/// long-lived write credential to the network in cleartext once per event
/// for the life of the tenant (CWE-319). Mixpanel had no equivalent
/// exposure: its token rode in the body of a request to one fixed `https`
/// address this crate chose, and no configuration could downgrade it.
///
/// Silence rather than a warning-and-send, because a warning is a line
/// nobody reads while the credential ships anyway, and a disclosed credential
/// cannot be un-disclosed once noticed. The reason names the variable and
/// the two ways out.
#[test]
fn a_cleartext_endpoint_is_silence_rather_than_a_credential_on_the_wire() {
    for insecure in [
        "http://collector.internal/track",
        "http://collector.internal:8080/track",
        "http://10.0.0.5:3000/track",
        "http://192.168.1.10/track",
        "http://[2001:db8::1]/track",
        "http://collector.example.com/api/track",
        // Not loopback, however much it looks like it.
        "http://127.0.0.1.evil.example/track",
        "http://localhost.evil.example/track",
    ] {
        let decision = resolve(
            Deployment::HostedTenant,
            &configured(&[(ENDPOINT_ENV, insecure)]),
        );
        assert_eq!(
            decision,
            Decision::Silent(Silence::InsecureEndpoint),
            "{insecure:?} would send the client id in the clear"
        );
        assert!(!decision.reports(), "{insecure:?}");
    }
}

/// The control that keeps the test above from passing by rejecting every
/// `http` URL: **loopback `http` is the documented exception and still
/// reports.**
///
/// It is not a concession — it is the only `http` case that is actually
/// safe, because that traffic does not leave the host and so never crosses
/// a network between machines. It is also
/// how the collector is run beside the workload in development, and how
/// every gated transport test in this crate points at its own local
/// collector; without this arm those tests would be asserting against a
/// tracker that resolve had already silenced.
#[test]
fn loopback_http_is_the_one_cleartext_endpoint_that_still_reports() {
    for loopback in [
        "http://127.0.0.1:3000/track",
        "http://127.0.0.1/track",
        "http://127.1.2.3:9/track",
        "http://[::1]:3000/track",
        "http://[::1]/track",
        "http://localhost:3000/track",
        "http://LocalHost:3000/track",
    ] {
        match resolve(
            Deployment::HostedTenant,
            &configured(&[(ENDPOINT_ENV, loopback)]),
        ) {
            Decision::Report { endpoint, .. } => assert_eq!(endpoint, loopback),
            other => panic!("{loopback:?} is loopback and must still report: {other:?}"),
        }
    }
}

/// The insecure reason names the variable and the fix, and — like every
/// other reason here — **never quotes the value**.
///
/// This one matters more than most: the endpoint it is rejecting is by
/// definition one an operator typed, and a self-hosted collector is
/// routinely fronted by an authenticated proxy that carries its key in the
/// URL. Quoting the rejected value would print that key in the boot line of
/// every tenant the new rule silences.
#[test]
fn the_insecure_endpoint_reason_never_quotes_the_endpoint() {
    const SECRET: &str = "NotARealCollectorKey";
    let reason = Silence::InsecureEndpoint.as_str();
    assert!(
        reason.contains("OPENCOMPANY_ANALYTICS_ENDPOINT"),
        "the reason must name the variable to act on: {reason}"
    );
    assert!(
        reason.contains("https") && reason.contains("loopback"),
        "the reason must name both ways out: {reason}"
    );

    let raw = format!("http://collector.internal/track?key={SECRET}");
    assert_eq!(
        resolve(
            Deployment::HostedTenant,
            &configured(&[(ENDPOINT_ENV, raw.as_str())])
        ),
        Decision::Silent(Silence::InsecureEndpoint)
    );
    let printed = format!("{:?} {}", Silence::InsecureEndpoint, reason);
    assert!(
        !printed
            .to_ascii_lowercase()
            .contains(&SECRET.to_ascii_lowercase()),
        "the reason leaked the endpoint credential: {printed}"
    );
    // The self-check: the needle really is findable in the unredacted
    // value, or the guard above is vacuous.
    assert!(
        raw.to_ascii_lowercase()
            .contains(&SECRET.to_ascii_lowercase()),
        "the needle must be findable before redaction: {raw}"
    );
}

/// **Shape is judged before transport security**, so the two reasons stay
/// distinguishable and each sends an operator to the edit it names.
///
/// `http://collector.internal:99999/track` is both unparseable *and* plain
/// http; it must be reported as unusable, because there is no host to judge
/// until it parses and "this will not parse" is the more actionable half.
#[test]
fn an_unparseable_cleartext_endpoint_is_unusable_rather_than_insecure() {
    for both in [
        "http://collector.internal:99999/track",
        "http://collector internal/track",
        "http://[::1/track",
    ] {
        assert_eq!(
            resolve(
                Deployment::HostedTenant,
                &configured(&[(ENDPOINT_ENV, both)])
            ),
            Decision::Silent(Silence::UnusableEndpoint),
            "{both:?} does not parse, so the reason must be about the parse"
        );
    }
}

/// The controls that keep the group above from passing by rejecting
/// everything: the endpoints a deployment actually uses still resolve, and
/// still resolve to themselves.
#[test]
fn a_usable_endpoint_still_reports_to_exactly_itself() {
    for usable in [
        TEST_ENDPOINT,
        "http://127.0.0.1:9/track",
        "http://127.0.0.1:9",
        "https://collector.internal/track",
        "HTTPS://collector.internal/track",
        "https://collector.internal/track?key=NotARealCollectorKey",
        "https://someone:NotARealCollectorKey@collector.internal/track",
        "https://[::1]:8443/track",
        "https://collector.internal:8443/track#frag",
    ] {
        match resolve(
            Deployment::HostedTenant,
            &configured(&[(ENDPOINT_ENV, usable)]),
        ) {
            Decision::Report { endpoint, .. } => assert_eq!(endpoint, usable),
            other => panic!("{usable:?} must still report: {other:?}"),
        }
    }
}

/// The credential must not be printable by accident, because the accident is
/// a `{:?}` in a log line nobody reviewed.
///
/// OpenPanel's own web SDK treats a client id as public, but with the
/// collector's secret check off the id is the whole write credential, and
/// there is no line in this tree that is better for carrying it.
#[test]
fn the_client_id_is_not_printable() {
    let credentials = ClientCredentials::new("not-a-real-client-id");
    let printed = format!("{credentials:?}");
    assert!(
        !printed.contains("not-a-real-client-id"),
        "the Debug impl leaked the client id: {printed}"
    );

    let decision = Decision::Report {
        endpoint: TEST_ENDPOINT.to_string(),
        credentials,
    };
    let printed = format!("{decision:?}");
    assert!(
        !printed.contains("not-a-real-client-id"),
        "the Debug impl leaked the client id through the decision: {printed}"
    );
}
