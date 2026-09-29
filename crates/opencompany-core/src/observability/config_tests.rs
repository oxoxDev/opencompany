use super::*;
use crate::app::config::MapEnv;

const DSN: &str = "https://examplePublicKey@o0.ingest.sentry.io/0";

fn resolved(pairs: &[(&str, &str)]) -> Decision {
    resolve(Deployment::SelfHosted, &MapEnv::new(pairs.to_vec()))
}

#[test]
fn an_install_that_configures_nothing_is_silent() {
    assert_eq!(resolved(&[]), Decision::Silent(Silence::NoDsn));
}

#[test]
fn a_configured_dsn_reports() {
    let Decision::Report {
        dsn,
        environment,
        release,
        traces,
    } = resolved(&[(DSN_ENV, DSN)])
    else {
        panic!("a configured DSN reports");
    };
    assert_eq!(dsn.expose(), DSN);
    assert_eq!(environment, "self-hosted");
    assert_eq!(release, release_tag());
    // Reporting errors is not agreeing to a per-request transaction feed.
    assert_eq!(traces, Traces::Off);
}

#[test]
fn performance_tracing_is_off_until_a_rate_is_asked_for() {
    for pairs in [
        vec![(DSN_ENV, DSN)],
        vec![(DSN_ENV, DSN), (TRACES_SAMPLE_RATE_ENV, "")],
        vec![(DSN_ENV, DSN), (TRACES_SAMPLE_RATE_ENV, "  ")],
        // An explicit zero reads the same as never having set it, because
        // the process does the same thing.
        vec![(DSN_ENV, DSN), (TRACES_SAMPLE_RATE_ENV, "0")],
        vec![(DSN_ENV, DSN), (TRACES_SAMPLE_RATE_ENV, "0.0")],
    ] {
        let Decision::Report { traces, .. } = resolved(&pairs) else {
            panic!("a configured DSN reports: {pairs:?}");
        };
        assert_eq!(traces, Traces::Off, "{pairs:?}");
        assert_eq!(traces.rate(), 0.0);
        assert!(!traces.is_on());
    }
}

#[test]
fn a_rate_between_zero_and_one_is_taken_as_asked() {
    for (raw, expected) in [("1", 1.0f32), ("1.0", 1.0), ("0.1", 0.1), (" 0.25 ", 0.25)] {
        let Decision::Report { traces, .. } =
            resolved(&[(DSN_ENV, DSN), (TRACES_SAMPLE_RATE_ENV, raw)])
        else {
            panic!("a configured DSN reports: {raw}");
        };
        assert_eq!(traces, Traces::Sampled(expected), "{raw}");
        assert!(traces.is_on(), "{raw}");
    }
}

#[test]
fn a_rate_that_is_not_a_fraction_is_refused_rather_than_clamped() {
    // `100` almost certainly means "100%", and clamping it to 1.0 would
    // record every request for an operator who meant nothing of the kind.
    // `-1`, `abc` and `50%` are typos. All of them land on `Off`, and all
    // of them say so in the boot line.
    for raw in [
        "100", "50%", "-1", "-0.5", "1.5", "abc", "0,5", "NaN", "inf",
    ] {
        let Decision::Report { traces, .. } =
            resolved(&[(DSN_ENV, DSN), (TRACES_SAMPLE_RATE_ENV, raw)])
        else {
            panic!("a configured DSN reports: {raw}");
        };
        assert_eq!(traces, Traces::Unreadable, "{raw}");
        assert_eq!(traces.rate(), 0.0, "{raw}");
        assert!(!traces.is_on(), "{raw}");
    }
}

#[test]
fn the_boot_line_says_what_tracing_will_do() {
    // An operator who set a rate and got a typo has to be able to tell that
    // from one who set nothing, which is the whole reason `Unreadable` is
    // a separate state.
    let off = resolved(&[(DSN_ENV, DSN)]).describe();
    assert!(off.contains("performance tracing off"), "{off}");

    let on = resolved(&[(DSN_ENV, DSN), (TRACES_SAMPLE_RATE_ENV, "0.1")]).describe();
    assert!(on.contains("tracing 10% of requests"), "{on}");

    let typo = resolved(&[(DSN_ENV, DSN), (TRACES_SAMPLE_RATE_ENV, "50%")]).describe();
    assert!(typo.contains("performance tracing off"), "{typo}");
    assert!(typo.contains(TRACES_SAMPLE_RATE_ENV), "{typo}");
}

#[test]
fn off_outranks_a_configured_dsn() {
    assert_eq!(
        resolved(&[(DSN_ENV, DSN), (ENABLE_ENV, "off")]),
        Decision::Silent(Silence::OptedOut)
    );
    // Case and surrounding whitespace are the operator's, not a typo.
    assert_eq!(
        resolved(&[(DSN_ENV, DSN), (ENABLE_ENV, "  OFF ")]),
        Decision::Silent(Silence::OptedOut)
    );
}

#[test]
fn on_does_not_conjure_a_destination() {
    // There is nothing for `on` to force: without a DSN there is nowhere to
    // send, and the reason must say that rather than "opted out".
    assert_eq!(
        resolved(&[(ENABLE_ENV, "on")]),
        Decision::Silent(Silence::NoDsn)
    );
}

#[test]
fn a_misspelt_switch_is_silence_and_says_so() {
    // The typo that matters: an operator who meant `off` and typed `of`
    // must not keep reporting. Both directions resolve to silence, and the
    // reason distinguishes them.
    let decision = resolved(&[(DSN_ENV, DSN), (ENABLE_ENV, "of")]);
    assert_eq!(decision, Decision::Silent(Silence::Unreadable));
    assert!(decision.describe().contains("not recognised"));
}

#[test]
fn a_blank_switch_is_absent_rather_than_unreadable() {
    // A launcher that exports an empty variable changes nothing.
    let Decision::Report { .. } = resolved(&[(DSN_ENV, DSN), (ENABLE_ENV, "")]) else {
        panic!("a blank switch leaves the DSN in charge");
    };
}

#[test]
fn a_blank_dsn_is_absent_rather_than_unusable() {
    assert_eq!(
        resolved(&[(DSN_ENV, "   ")]),
        Decision::Silent(Silence::NoDsn)
    );
}

#[test]
fn shapes_that_are_not_a_dsn_are_refused() {
    // Each of these parses as *something*, or nearly does, and each would
    // resolve to a client that never delivers.
    for candidate in [
        // No scheme — how anyone writes a proxy host the first time.
        "o0.ingest.sentry.io/0",
        // A scheme nothing can POST to.
        "ftp://key@o0.ingest.sentry.io/0",
        // No public key.
        "https://o0.ingest.sentry.io/0",
        // No project id.
        "https://key@o0.ingest.sentry.io/",
        // No host.
        "https://key@/0",
        // The pre-2016 secret-half form: no ingest accepts it, so this is
        // either stale or a credential pasted into the wrong variable.
        "https://key:secret@o0.ingest.sentry.io/0",
        // Not a URL at all.
        "not a dsn",
        "",
    ] {
        assert_eq!(
            resolve(Deployment::SelfHosted, &MapEnv::new([(DSN_ENV, candidate)])),
            match candidate {
                "" => Decision::Silent(Silence::NoDsn),
                _ => Decision::Silent(Silence::UnusableDsn),
            },
            "{candidate} must not resolve to reporting"
        );
    }
}

#[test]
fn the_environment_tag_defaults_to_the_deployment_kind() {
    for (deployment, expected) in [
        (Deployment::Desktop, "desktop"),
        (Deployment::SelfHosted, "self-hosted"),
        (Deployment::HostedTenant, "hosted-tenant"),
    ] {
        let Decision::Report { environment, .. } =
            resolve(deployment, &MapEnv::new([(DSN_ENV, DSN)]))
        else {
            panic!("a configured DSN reports");
        };
        assert_eq!(environment, expected);
    }
}

#[test]
fn the_environment_tag_is_overridable_and_normalized() {
    let Decision::Report { environment, .. } = resolve(
        Deployment::HostedTenant,
        &MapEnv::new([(DSN_ENV, DSN), (ENVIRONMENT_ENV, "  Staging ")]),
    ) else {
        panic!("a configured DSN reports");
    };
    assert_eq!(environment, "staging");
}

#[test]
fn the_boot_line_never_carries_the_public_key() {
    let decision = resolved(&[(DSN_ENV, DSN)]);
    let line = decision.describe();
    assert!(!line.contains("examplePublicKey"), "{line}");
    assert!(line.contains("https://o0.ingest.sentry.io/0"), "{line}");
    assert!(line.contains("self-hosted"), "{line}");
}

#[test]
fn a_dsn_never_prints_itself() {
    // `{:?}` is how a credential reaches a log without anyone deciding to
    // put it there — a `dbg!`, a `#[derive(Debug)]` on a struct that holds
    // one, a `tracing::error!(?config)`.
    let Decision::Report { dsn, .. } = resolved(&[(DSN_ENV, DSN)]) else {
        panic!("a configured DSN reports");
    };
    assert_eq!(format!("{dsn:?}"), "Dsn(<redacted>)");
    let debugged = format!("{:?}", resolved(&[(DSN_ENV, DSN)]));
    assert!(!debugged.contains("examplePublicKey"), "{debugged}");
}

#[test]
fn a_proxied_dsn_loses_its_query_string_too() {
    // An ingest fronted by an authenticated proxy carries its key in the
    // two places a URL can hold one. Both have to go.
    let dsn = Dsn::new("https://key@proxy.internal/api/2?auth=hunter2#frag");
    let loggable = dsn.loggable();
    assert!(!loggable.contains("key@"), "{loggable}");
    assert!(!loggable.contains("hunter2"), "{loggable}");
    assert_eq!(loggable, "https://proxy.internal/api/2");
}

#[test]
fn the_release_tag_names_the_commit_when_the_build_knows_one() {
    assert_eq!(
        release_tag_from("0.1.0", "d31e532f7c8a"),
        "opencompany@0.1.0+d31e532f7c8a"
    );
    // A modified tree is a different build and the tag says so.
    assert_eq!(
        release_tag_from("0.1.0", "d31e532f7c8a-dirty"),
        "opencompany@0.1.0+d31e532f7c8a-dirty"
    );
    // `unknown` is dropped rather than appended: a release name that looks
    // like a commit and is not one is worse than an honest absence.
    assert_eq!(release_tag_from("0.1.0", "unknown"), "opencompany@0.1.0");
    assert_eq!(release_tag_from("0.1.0", "  "), "opencompany@0.1.0");
}

#[test]
fn the_real_release_tag_is_well_formed() {
    let tag = release_tag();
    assert!(tag.starts_with("opencompany@"), "{tag}");
    assert!(tag.contains(crate::VERSION), "{tag}");
    assert!(!tag.contains("unknown"), "{tag}");
}

#[test]
fn every_silence_names_a_reason() {
    for reason in [
        Silence::OptedOut,
        Silence::NoDsn,
        Silence::Unreadable,
        Silence::UnusableDsn,
        Silence::NotCompiled,
    ] {
        let line = Decision::Silent(reason).describe();
        assert!(line.starts_with("crash reporting: off ("), "{line}");
        assert!(line.ends_with(')'), "{line}");
    }
}

fn hosted(pairs: &[(&str, &str)]) -> Decision {
    resolve(Deployment::HostedTenant, &MapEnv::new(pairs.to_vec()))
}

fn destination(decision: &Decision) -> Option<&str> {
    match decision {
        Decision::Report { dsn, .. } => Some(dsn.expose()),
        Decision::Silent(_) => None,
    }
}

/// A hosted tenant with no DSN (unset or blank) reports to the compiled-in
/// TinyHumans project, tagged as a hosted tenant.
#[test]
fn a_hosted_tenant_without_a_dsn_uses_the_default() {
    for pairs in [&[][..], &[(DSN_ENV, "  ")][..]] {
        let decision = hosted(pairs);
        assert_eq!(destination(&decision), Some(DEFAULT_HOSTED_TENANT_DSN));
        match decision {
            Decision::Report { environment, .. } => assert_eq!(environment, "hosted-tenant"),
            other => panic!("{other:?}"),
        }
    }
}

/// The default never reaches desktop or self-hosted, both of which can
/// compile `crash-reporting`.
#[test]
fn the_default_dsn_is_hosted_tenant_only() {
    for deployment in [Deployment::Desktop, Deployment::SelfHosted] {
        assert_eq!(
            resolve(deployment, &MapEnv::new(Vec::<(&str, &str)>::new())),
            Decision::Silent(Silence::NoDsn),
            "{deployment:?}"
        );
    }
}

/// The runtime DSN outranks the default, a malformed one is reported rather
/// than replaced, and `off` silences the default too.
#[test]
fn the_default_dsn_yields_to_the_operator() {
    assert_eq!(destination(&hosted(&[(DSN_ENV, DSN)])), Some(DSN));
    assert_eq!(
        hosted(&[(DSN_ENV, "not a dsn")]),
        Decision::Silent(Silence::UnusableDsn)
    );
    assert_eq!(
        hosted(&[(ENABLE_ENV, "off")]),
        Decision::Silent(Silence::OptedOut)
    );
}

/// The compiled-in constant is itself a DSN this module accepts.
#[test]
fn the_default_dsn_parses() {
    assert!(parse_dsn(DEFAULT_HOSTED_TENANT_DSN).is_some());
}
