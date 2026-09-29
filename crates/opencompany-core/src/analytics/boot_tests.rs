use super::*;
use crate::analytics::config::{
    CLIENT_ID_ENV, ClientCredentials, ENABLE_ENV, ENDPOINT_ENV, Silence,
};
use crate::app::config::MapEnv;
use crate::app::deployment::DEPLOYMENT_ENV;
use crate::{AppConfig, AppState};

/// A collector address that resolves nowhere. Named in every reporting
/// environment below: there is no default endpoint any more, so an
/// environment without one resolves to `NoEndpoint` rather than to
/// reporting.
const TEST_ENDPOINT: &str = "https://collector.invalid/track";

/// The two variables a reporting deployment configures.
fn credential_env(pairs: &[(&str, &str)]) -> MapEnv {
    let mut all = vec![
        (CLIENT_ID_ENV, "not-a-real-client-id"),
        (ENDPOINT_ENV, TEST_ENDPOINT),
    ];
    all.extend_from_slice(pairs);
    MapEnv::new(all)
}

/// A reporting decision, for the pure `describe` tests.
fn reporting(endpoint: &str) -> Decision {
    Decision::Report {
        endpoint: endpoint.to_string(),
        credentials: ClientCredentials::new("not-a-real-client-id"),
    }
}

fn state() -> (AppState, tempfile::TempDir) {
    let home = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(AppConfig::default()).with_home(home.path());
    (state, home)
}

/// **The default posture, asserted end to end at the boot seam.** A host
/// that says nothing installs a tracker that sends nothing, even with a
/// working credential and a collector sitting in its environment.
#[test]
fn an_undeclared_host_installs_silence() {
    let (state, _home) = state();
    let handle = DeferredTracker::new();
    let decision = install(&state, &handle, &credential_env(&[]));
    assert_eq!(decision, Decision::Silent(Silence::NotHosted));
    assert_eq!(
        describe(&decision),
        "analytics: off (not a hosted tenant and no explicit opt-in)"
    );
}

/// A hosted tenant resolves to reporting. In a build without the `analytics`
/// feature the installed tracker is still a no-op — the decision is the same
/// either way, which is what this pins.
#[test]
fn a_hosted_tenant_resolves_to_reporting() {
    let (state, _home) = state();
    let handle = DeferredTracker::new();
    let decision = install(
        &state,
        &handle,
        &credential_env(&[(DEPLOYMENT_ENV, "hosted-tenant")]),
    );
    assert!(decision.reports(), "{decision:?}");
    assert!(
        !describe(&decision).contains("not-a-real-client-id"),
        "the boot line must not carry the client id: {}",
        describe(&decision)
    );
}

/// **The boot line reports behaviour, not configuration.** A build with no
/// `analytics` feature installs a `NullTracker` for a reporting decision, so
/// the line must not claim it is reporting; a build with the feature must.
/// The two halves are asserted from one `cfg!`, so the default lane and the
/// scoped `analytics` lane each exercise their own branch and neither can
/// pass by ignoring the build.
#[test]
fn the_boot_line_says_when_the_build_has_no_transport() {
    let decision = reporting(TEST_ENDPOINT);
    let line = describe(&decision);
    assert!(!line.contains("not-a-real-client-id"), "{line}");

    if cfg!(feature = "analytics") {
        assert_eq!(
            line,
            "analytics: reporting to https://collector.invalid/track"
        );
    } else {
        assert!(
            line.starts_with("analytics: off ("),
            "a build with no transport must not read as reporting: {line}"
        );
        assert!(line.contains("without the `analytics` feature"), "{line}");
        assert!(
            line.contains("https://collector.invalid/track"),
            "the configured endpoint is still named, so the operator can see \
             what was intended: {line}"
        );
    }
}

/// **A credential in the endpoint must not reach a log line.**
///
/// `OPENCOMPANY_ANALYTICS_ENDPOINT` names the collector the operator
/// self-hosts, which is routinely reached through an authenticated proxy,
/// and such a proxy carries its key in userinfo or in the query string.
/// `ClientCredentials`' redaction guards two different strings entirely and
/// does nothing here.
///
/// Asserted case-insensitively: a redaction that merely lowercased the
/// secret would still have leaked it, and an exact-case search would read
/// that as clean.
#[test]
fn a_credential_in_the_endpoint_never_reaches_the_boot_line() {
    for raw in [
        "https://someone:NotARealCollectorKey@collector.invalid/track",
        "https://collector.invalid/track?key=NotARealCollectorKey",
        "https://collector.invalid/track#NotARealCollectorKey",
        "https://someone:NotARealCollectorKey@collector.invalid/t?k=NotARealCollectorKey",
        // The third place a URL can carry one, which userinfo and query
        // stripping both miss: a signed path segment.
        "https://collector.invalid/ingest/NotARealCollectorKey",
        "https://collector.invalid/v1/ingest/NotARealCollectorKey/track",
    ] {
        let line = describe(&reporting(raw));
        assert!(
            !line
                .to_ascii_lowercase()
                .contains(&SECRET.to_ascii_lowercase()),
            "the boot line leaked the endpoint credential in {raw:?}: {line}"
        );
        assert!(
            line.contains("collector.invalid"),
            "the destination is still named, or the line is useless: {line}"
        );
        assert!(
            line.contains("credentials redacted"),
            "a shortened URL must say it was shortened: {line}"
        );
    }
}

/// A credential-shaped value that stands in for a collector proxy's key.
/// Mixed case on purpose: see the self-check below.
const SECRET: &str = "NotARealCollectorKey";

/// The self-check for the assertion above. A leak guard that cannot find
/// the needle in an **unredacted** value proves nothing about the redacted
/// one — the needle may simply never have been there, or the comparison may
/// be case-sensitive against a value something lowercased on the way
/// through. Both have happened on this PR.
#[test]
fn the_leak_assertion_would_catch_an_unredacted_endpoint() {
    let unredacted = format!("https://collector.invalid/track?key={SECRET}");
    assert!(
        unredacted
            .to_ascii_lowercase()
            .contains(&SECRET.to_ascii_lowercase()),
        "the needle must be findable before redaction, or the guard is vacuous"
    );
    assert!(
        unredacted
            .to_ascii_lowercase()
            .contains(&SECRET.to_ascii_uppercase().to_ascii_lowercase()),
        "and findable whatever case it comes back in"
    );
}

/// An ordinary endpoint is printed unchanged and says nothing about
/// redaction. The control: without it, "redact everything" would pass.
#[test]
fn an_ordinary_endpoint_is_printed_unchanged() {
    let line = describe(&reporting(TEST_ENDPOINT));
    assert!(line.contains(TEST_ENDPOINT), "{line}");
    assert!(!line.contains("credentials redacted"), "{line}");
}

/// An unreadable switch says so at boot, rather than looking like a
/// deliberate opt-out or like a working opt-in.
#[test]
fn an_unreadable_switch_says_so() {
    let (state, _home) = state();
    let handle = DeferredTracker::new();
    let decision = install(
        &state,
        &handle,
        &credential_env(&[(DEPLOYMENT_ENV, "hosted-tenant"), (ENABLE_ENV, "of")]),
    );
    assert_eq!(decision, Decision::Silent(Silence::Unreadable));
    assert!(describe(&decision).contains("not recognised"));
}

/// A tenant state, for the identity tests below.
fn tenant_state(tenant: &str) -> (AppState, tempfile::TempDir) {
    let home = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(AppConfig {
        tenant_namespace: Some(tenant.into()),
        ..AppConfig::default()
    })
    .with_home(home.path());
    (state, home)
}

/// **A hosted tenant with no identity key is known by its instance id, not
/// by a digest of its slug.**
///
/// The dangerous fallback is the other one. An unkeyed `SHA-256(slug)` is
/// not an opaque identity: a slug is usually the customer's brand, so
/// whoever holds the digests can enumerate candidates and read the customer
/// straight back out. When the platform has not supplied a key there is
/// nothing private to derive, so the host names *itself* — 128 random bits
/// that identify nobody's customer — rather than naming its customer badly.
///
/// Asserted on what `identify` actually returns. An earlier version of this
/// test recomputed the expected id itself and compared the two, which is a
/// tautology: a mutation replacing the keyless path with a baked-in salt
/// passed it.
#[test]
fn a_tenant_without_an_identity_key_falls_back_to_the_instance_id() {
    let (state, _home) = tenant_state("acmecorp-holdings");
    let chosen = identify(
        &state,
        &credential_env(&[(DEPLOYMENT_ENV, "hosted-tenant")]),
    );

    assert_eq!(
        chosen.as_str(),
        OpaqueId::instance(state.instance_id()).as_str(),
        "a keyless tenant must be known by its own random instance id"
    );
    assert!(
        chosen.as_str().starts_with("i_"),
        "and never by anything in the tenant id space: {chosen:?}"
    );
}

/// And with a key the tenant *is* identified as a tenant — the control,
/// without which "it always uses the instance id now" would pass the test
/// above just as well.
#[test]
fn a_tenant_with_an_identity_key_is_identified_as_one() {
    let (state, _home) = tenant_state("acmecorp-holdings");
    let chosen = identify(
        &state,
        &credential_env(&[
            (DEPLOYMENT_ENV, "hosted-tenant"),
            (crate::analytics::config::ID_KEY_ENV, "not-a-real-id-key"),
        ]),
    );

    let key =
        crate::analytics::types::TenantIdKey::new("not-a-real-id-key").expect("a non-blank key");
    assert_eq!(
        chosen.as_str(),
        OpaqueId::tenant("acmecorp-holdings", &key).as_str(),
        "a keyed tenant is identified by its keyed digest"
    );
    assert!(chosen.as_str().starts_with("t_"), "{chosen:?}");
    assert_ne!(
        chosen.as_str(),
        OpaqueId::instance(state.instance_id()).as_str(),
        "the two id spaces must not collide"
    );
}

/// A blank key is not a key: it must send the tenant to the instance id
/// rather than deriving under a key everybody can guess.
#[test]
fn a_blank_identity_key_does_not_identify_a_tenant() {
    let (state, _home) = tenant_state("acmecorp-holdings");
    for blank in ["", "   ", "\n"] {
        let chosen = identify(
            &state,
            &credential_env(&[
                (DEPLOYMENT_ENV, "hosted-tenant"),
                (crate::analytics::config::ID_KEY_ENV, blank),
            ]),
        );
        assert_eq!(
            chosen.as_str(),
            OpaqueId::instance(state.instance_id()).as_str(),
            "a key of {blank:?} must read as absent"
        );
    }
}

/// The instance id is what a host with no tenant namespace is known by, and
/// it is prefixed so it can never be confused with a tenant digest.
#[test]
fn an_untenanted_host_is_known_by_its_instance_id() {
    let (state, _home) = state();
    let expected = OpaqueId::instance(state.instance_id());
    assert!(expected.as_str().starts_with("i_"));
    assert!(expected.as_str().contains(state.instance_id()));
}

#[test]
fn an_opted_out_host_says_why() {
    let (state, _home) = state();
    let handle = DeferredTracker::new();
    let decision = install(
        &state,
        &handle,
        &credential_env(&[(DEPLOYMENT_ENV, "hosted-tenant"), (ENABLE_ENV, "off")]),
    );
    assert_eq!(decision, Decision::Silent(Silence::OptedOut));
    assert!(describe(&decision).contains("operator opted out"));
}
