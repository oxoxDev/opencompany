//! Credential-presence and reporting-toggle tests, split out of
//! `config_tests.rs` (topic split, >750 lines).

use super::*;
use crate::app::config::MapEnv;

/// A collector address that resolves nowhere. Every reporting test needs
/// one now: there is no default endpoint left to fall back to.
const TEST_ENDPOINT: &str = "https://collector.invalid/track";

/// A fully configured reporting environment, which `pairs` then overrides.
///
/// It takes two variables, and that is the whole tenant contract: an
/// OpenPanel deployment configures a client id and the address of the
/// collector it self-hosts. There is no client secret — the collector's
/// clients run with "ignore CORS and secret".
fn configured(pairs: &[(&str, &str)]) -> MapEnv {
    let mut all = vec![
        (CLIENT_ID_ENV, "not-a-real-client-id"),
        (ENDPOINT_ENV, TEST_ENDPOINT),
    ];
    all.extend_from_slice(pairs);
    MapEnv::new(all)
}

/// **The decision the GPL posture rests on.** A self-hosted instance that
/// has been handed a working credential — which is the easiest way to get
/// this wrong, because a credential looks like consent — still sends
/// nothing.
#[test]
fn a_self_hosted_instance_is_silent_even_with_a_credential() {
    assert_eq!(
        resolve(Deployment::SelfHosted, &configured(&[])),
        Decision::Silent(Silence::NotHosted)
    );
}

#[test]
fn a_desktop_instance_is_silent_even_with_a_credential() {
    assert_eq!(
        resolve(Deployment::Desktop, &configured(&[])),
        Decision::Silent(Silence::NotHosted)
    );
}

#[test]
fn a_hosted_tenant_with_a_credential_reports() {
    let decision = resolve(Deployment::HostedTenant, &configured(&[]));
    assert!(decision.reports(), "{decision:?}");
    match decision {
        Decision::Report {
            endpoint,
            credentials,
        } => {
            assert_eq!(endpoint, TEST_ENDPOINT);
            assert_eq!(credentials.expose_id(), "not-a-real-client-id");
        }
        other => panic!("{other:?}"),
    }
}

/// **The client id alone is enough.** A leftover
/// `OPENCOMPANY_ANALYTICS_CLIENT_SECRET` from before the collector's secret
/// check was switched off is neither required nor read: its absence does not
/// silence a tenant, and its presence changes nothing.
#[test]
fn a_client_secret_is_neither_required_nor_read() {
    let id_only = resolve(Deployment::HostedTenant, &configured(&[]));
    assert!(id_only.reports(), "{id_only:?}");
    let with_leftover = resolve(
        Deployment::HostedTenant,
        &configured(&[("OPENCOMPANY_ANALYTICS_CLIENT_SECRET", "not a header\nvalue")]),
    );
    assert_eq!(with_leftover, id_only);
}

/// **A hosted tenant with nothing configured reports to the TinyHumans
/// collector as the TinyHumans client** — the compiled-in defaults, so the
/// tenant image needs no injected analytics configuration.
#[test]
fn a_hosted_tenant_without_configuration_uses_the_defaults() {
    match resolve(Deployment::HostedTenant, &MapEnv::default()) {
        Decision::Report {
            endpoint,
            credentials,
        } => {
            assert_eq!(endpoint, DEFAULT_ENDPOINT);
            assert_eq!(credentials.expose_id(), DEFAULT_CLIENT_ID);
        }
        other => panic!("{other:?}"),
    }
    // Blank is absent for both, and configuration outranks both defaults.
    match resolve(
        Deployment::HostedTenant,
        &MapEnv::new([(CLIENT_ID_ENV, " \n"), (ENDPOINT_ENV, "  ")]),
    ) {
        Decision::Report {
            endpoint,
            credentials,
        } => {
            assert_eq!(endpoint, DEFAULT_ENDPOINT);
            assert_eq!(credentials.expose_id(), DEFAULT_CLIENT_ID);
        }
        other => panic!("{other:?}"),
    }
    match resolve(Deployment::HostedTenant, &configured(&[])) {
        Decision::Report {
            endpoint,
            credentials,
        } => {
            assert_eq!(endpoint, TEST_ENDPOINT);
            assert_eq!(credentials.expose_id(), "not-a-real-client-id");
        }
        other => panic!("{other:?}"),
    }
    // `off` still wins over the defaults.
    assert_eq!(
        resolve(
            Deployment::HostedTenant,
            &MapEnv::new([(ENABLE_ENV, "off")])
        ),
        Decision::Silent(Silence::OptedOut)
    );
}

/// A set-but-malformed endpoint is reported, never papered over by the
/// default.
#[test]
fn a_malformed_endpoint_is_not_replaced_by_the_default() {
    assert_eq!(
        resolve(
            Deployment::HostedTenant,
            &MapEnv::new([(ENDPOINT_ENV, "collector.internal/track")])
        ),
        Decision::Silent(Silence::UnusableEndpoint)
    );
}

/// The compiled-in defaults pass the same validation configuration does.
#[test]
fn the_defaults_are_themselves_valid() {
    assert!(is_usable_endpoint(DEFAULT_ENDPOINT));
    assert!(is_secure_endpoint(DEFAULT_ENDPOINT));
    assert!(is_header_safe(DEFAULT_CLIENT_ID));
}

/// The reason names the variable to set, and a blank id is no id: a value
/// mounted from a file arrives with a trailing newline more often than not,
/// and a launcher that exports an empty variable has configured nothing.
#[test]
fn a_missing_or_blank_client_id_names_the_variable() {
    assert!(
        Silence::NoClientId
            .as_str()
            .contains("OPENCOMPANY_ANALYTICS_CLIENT_ID"),
        "the reason must name the variable to set: {}",
        Silence::NoClientId.as_str()
    );
    for blank in ["   ", "\n", "\t\n "] {
        assert_eq!(
            resolve(
                Deployment::SelfHosted,
                &configured(&[(CLIENT_ID_ENV, blank), (ENABLE_ENV, "on")])
            ),
            Decision::Silent(Silence::NoClientId),
            "an id of {blank:?} must not read as configured"
        );
    }
}

/// And a credential that merely *arrived* with surrounding whitespace is
/// used, trimmed, rather than put into a header with a newline in it — which
/// `reqwest` rejects outright when it builds the request.
#[test]
fn a_credential_is_trimmed() {
    match resolve(
        Deployment::HostedTenant,
        &configured(&[(CLIENT_ID_ENV, "  not-a-real-client-id\n")]),
    ) {
        Decision::Report { credentials, .. } => {
            assert_eq!(credentials.expose_id(), "not-a-real-client-id");
        }
        other => panic!("{other:?}"),
    }
}

/// **A credential that cannot go in a header is silence with a reason.**
///
/// This is new with OpenPanel and is a consequence of where the credential
/// now travels. Mixpanel's token rode in the JSON body, where any string is
/// legal, so a mangled one was simply refused by the collector. The client
/// id rides in the `openpanel-client-id` header, and `reqwest` refuses to
/// *build* a request whose header value holds a control byte — so an id
/// with an embedded newline (`kubectl create secret` over a wrapped file is
/// the usual way one arrives) would install a tracker that never constructs a single request, forever, behind a `debug!` nobody has
/// enabled. Trimming does not save it: the newline is in the middle.
#[test]
fn a_credential_that_cannot_go_in_a_header_is_silence() {
    for mangled in [
        "not-a-real\nclient-id",
        "not-a-real\rclient-id",
        "not a real client id",
        "not-a-real-client-id\u{0}",
        "not-a-r\u{e9}al-client-id",
    ] {
        assert_eq!(
            resolve(
                Deployment::HostedTenant,
                &configured(&[(CLIENT_ID_ENV, mangled)])
            ),
            Decision::Silent(Silence::UnusableCredential),
            "an id of {mangled:?} must not resolve to a report that cannot be built"
        );
    }

    // The control, without which "reject everything" would pass: the shapes
    // an OpenPanel client actually has still report. Opaque generated
    // tokens — hex, base64url, a uuid, a prefixed key.
    for real_shaped in [
        "0f8b1c2d3e4f5a6b7c8d9e0f1a2b3c4d",
        "op_sk_9zQx-4Kd_7Yb2Lp0",
        "550e8400-e29b-41d4-a716-446655440000",
        "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXo=",
    ] {
        assert!(
            resolve(
                Deployment::HostedTenant,
                &configured(&[(CLIENT_ID_ENV, real_shaped)])
            )
            .reports(),
            "{real_shaped:?} is the shape a real credential has and must still report"
        );
    }
}

/// And the reason never quotes the credential it rejected, for the same
/// reason the endpoint reason does not quote the endpoint.
#[test]
fn the_unusable_credential_reason_never_quotes_the_credential() {
    let reason = Silence::UnusableCredential.as_str();
    let printed = format!("{:?} {reason}", Silence::UnusableCredential);
    assert!(
        !printed.to_ascii_lowercase().contains("not-a-real"),
        "the reason leaked the credential: {printed}"
    );
    assert!(
        reason.contains("header"),
        "the reason must say what is wrong with it: {reason}"
    );
}

/// **Outside a hosted tenant there is no default, and an absent endpoint
/// or id is silence with its own reason.**
///
/// A self-hoster who opts in with `OPENCOMPANY_ANALYTICS=on` has asked to
/// report to *their* collector; the TinyHumans default would be somebody
/// else's, so it never applies to them.
#[test]
fn an_absent_endpoint_is_silence_rather_than_a_default() {
    let decision = resolve(
        Deployment::SelfHosted,
        &MapEnv::new([(CLIENT_ID_ENV, "not-a-real-client-id"), (ENABLE_ENV, "on")]),
    );
    assert_eq!(decision, Decision::Silent(Silence::NoEndpoint));
    assert!(!decision.reports());
    assert!(
        Silence::NoEndpoint
            .as_str()
            .contains("OPENCOMPANY_ANALYTICS_ENDPOINT"),
        "the reason must name the variable to set: {}",
        Silence::NoEndpoint.as_str()
    );
    assert_eq!(
        resolve(
            Deployment::SelfHosted,
            &MapEnv::new([(ENDPOINT_ENV, TEST_ENDPOINT), (ENABLE_ENV, "on")]),
        ),
        Decision::Silent(Silence::NoClientId)
    );
}

/// A blank endpoint is an absent one, not a broken one: a launcher that
/// exports an empty variable has configured nothing, and the reason it gets
/// should send it to set the variable rather than to fix its value.
#[test]
fn a_blank_endpoint_is_absent_rather_than_unusable() {
    assert_eq!(
        resolve(
            Deployment::SelfHosted,
            &configured(&[(ENDPOINT_ENV, "  \n"), (ENABLE_ENV, "on")])
        ),
        Decision::Silent(Silence::NoEndpoint)
    );
}

/// `off` outranks the deployment kind. The platform can switch a tenant off
/// without rebuilding it.
#[test]
fn off_outranks_a_hosted_deployment() {
    assert_eq!(
        resolve(
            Deployment::HostedTenant,
            &configured(&[(ENABLE_ENV, "off")])
        ),
        Decision::Silent(Silence::OptedOut)
    );
}

/// The self-hoster's opt-in, which is the only way a non-hosted install ever
/// reports.
#[test]
fn a_self_hoster_can_opt_in() {
    assert!(resolve(Deployment::SelfHosted, &configured(&[(ENABLE_ENV, "on")])).reports());
}

/// A typo must not opt anybody in.
#[test]
fn a_misspelled_switch_does_not_opt_in() {
    assert_eq!(
        resolve(Deployment::SelfHosted, &configured(&[(ENABLE_ENV, "onn")])),
        Decision::Silent(Silence::Unreadable)
    );
}

/// **And a typo must not fail to opt anybody out.** This is the direction
/// that used to leak: an unreadable value fell through to the deployment
/// default, so a hosted tenant whose operator meant `off` and typed `of`
/// carried on reporting, with a boot line that said "reporting to …" and
/// gave them no reason to look again.
#[test]
fn a_misspelled_opt_out_does_not_keep_a_hosted_tenant_reporting() {
    for typo in ["of", "offf", "disabled", "0.0", "nope"] {
        let decision = resolve(Deployment::HostedTenant, &configured(&[(ENABLE_ENV, typo)]));
        assert_eq!(
            decision,
            Decision::Silent(Silence::Unreadable),
            "{typo:?} must not leave a hosted tenant reporting"
        );
        assert!(!decision.reports(), "{typo:?}");
    }
}

/// **A switch that is set but is not text fails closed too.**
///
/// `EnvSource::get` maps a non-Unicode value to `None`, so reading through
/// it would have treated `OPENCOMPANY_ANALYTICS=<invalid bytes>` as an
/// absent switch and left a hosted tenant reporting — the same leak as the
/// unreadable spelling, by a different route.
#[cfg(unix)]
#[test]
fn a_non_unicode_switch_is_unreadable_rather_than_absent() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    struct NonUnicodeSwitch;
    impl EnvSource for NonUnicodeSwitch {
        fn get_os(&self, key: &str) -> Option<OsString> {
            match key {
                ENABLE_ENV => Some(OsString::from_vec(vec![0xff, 0xfe, 0x6f, 0x6e])),
                CLIENT_ID_ENV => Some(OsString::from("not-a-real-client-id")),
                ENDPOINT_ENV => Some(OsString::from(TEST_ENDPOINT)),
                _ => None,
            }
        }
    }

    // The premise: this really is a value `get` cannot see at all.
    assert_eq!(NonUnicodeSwitch.get(ENABLE_ENV), None);
    assert!(NonUnicodeSwitch.get_os(ENABLE_ENV).is_some());

    assert_eq!(
        resolve(Deployment::HostedTenant, &NonUnicodeSwitch),
        Decision::Silent(Silence::Unreadable),
        "a switch set to bytes this process cannot read must not read as unset"
    );
}

/// **A client id that is set but is not text fails closed too.**
///
/// `non_blank` used to read the credential through [`EnvSource::get`], which
/// maps a non-Unicode value to `None`. For a hosted tenant that `None` then
/// fell to [`DEFAULT_CLIENT_ID`] — reporting under the compiled-in credential
/// for a tenant that *did* configure one, just not one this process could
/// read, rather than the `Silence::UnusableCredential` a malformed value
/// should produce. Same leak as the switch above, by a different route.
#[cfg(unix)]
#[test]
fn a_non_unicode_client_id_is_unusable_rather_than_the_default() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    struct NonUnicodeClientId;
    impl EnvSource for NonUnicodeClientId {
        fn get_os(&self, key: &str) -> Option<OsString> {
            match key {
                CLIENT_ID_ENV => Some(OsString::from_vec(vec![0xff, 0xfe])),
                ENDPOINT_ENV => Some(OsString::from(TEST_ENDPOINT)),
                _ => None,
            }
        }
    }

    // The premise: this really is a value `get` cannot see at all.
    assert_eq!(NonUnicodeClientId.get(CLIENT_ID_ENV), None);
    assert!(NonUnicodeClientId.get_os(CLIENT_ID_ENV).is_some());

    let decision = resolve(Deployment::HostedTenant, &NonUnicodeClientId);
    assert_eq!(
        decision,
        Decision::Silent(Silence::UnusableCredential),
        "a client id set to bytes this process cannot read must not fall back to the default"
    );
    assert!(!decision.reports());
}

/// The near-miss control: `off` really is matched case-insensitively and
/// after trimming, so the test above is finding typos rather than finding
/// every value that is not lowercase and bare.
#[test]
fn an_off_switch_is_trimmed_and_case_folded() {
    assert_eq!(
        resolve(
            Deployment::HostedTenant,
            &configured(&[(ENABLE_ENV, "  ofF\n")])
        ),
        Decision::Silent(Silence::OptedOut)
    );
}

/// The control for the two above: an **absent** switch still falls to the
/// deployment default, in both directions. Without this, "everything is
/// silent now" would pass the tests above just as well.
#[test]
fn an_absent_switch_still_falls_to_the_deployment_default() {
    assert!(resolve(Deployment::HostedTenant, &configured(&[])).reports());
    assert_eq!(
        resolve(Deployment::SelfHosted, &configured(&[])),
        Decision::Silent(Silence::NotHosted)
    );
}

/// A whitespace-only switch is an absent switch, not an unreadable one —
/// consistent with the credential and endpoint, and it must not flip a
/// hosted tenant into silence just because a launcher exported an empty
/// variable.
#[test]
fn a_blank_switch_is_treated_as_absent() {
    assert!(
        resolve(
            Deployment::HostedTenant,
            &configured(&[(ENABLE_ENV, "   ")])
        )
        .reports(),
        "a blank switch must not read as unreadable"
    );
}
