use super::*;
use opencompany::app::config::MapEnv;
use opencompany::app::deployment::Deployment;
use opencompany::observability::config::{ENABLE_ENV, resolve};
use opencompany::observability::{Decision, Silence};

const BAKED: &str = "https://publickey@sentry.example.test/14";
const RUNTIME: &str = "https://otherkey@sentry.example.test/10";

fn destination(decision: &Decision) -> Option<String> {
    match decision {
        Decision::Report { dsn, .. } => Some(dsn.expose().to_string()),
        Decision::Silent(_) => None,
    }
}

#[test]
fn baked_dsn_fills_an_absent_runtime_dsn() {
    let env = DesktopEnv::with_baked(MapEnv::new(Vec::<(&str, &str)>::new()), Some(BAKED));
    assert_eq!(
        destination(&resolve(Deployment::Desktop, &env)).as_deref(),
        Some(BAKED)
    );
}

#[test]
fn baked_dsn_fills_a_blank_runtime_dsn() {
    let env = DesktopEnv::with_baked(MapEnv::new([(DSN_ENV, "  ")]), Some(BAKED));
    assert_eq!(
        destination(&resolve(Deployment::Desktop, &env)).as_deref(),
        Some(BAKED)
    );
}

#[test]
fn runtime_dsn_outranks_the_baked_one() {
    let env = DesktopEnv::with_baked(MapEnv::new([(DSN_ENV, RUNTIME)]), Some(BAKED));
    assert_eq!(
        destination(&resolve(Deployment::Desktop, &env)).as_deref(),
        Some(RUNTIME)
    );
}

#[test]
fn a_malformed_runtime_dsn_is_reported_not_replaced() {
    let env = DesktopEnv::with_baked(MapEnv::new([(DSN_ENV, "not a dsn")]), Some(BAKED));
    assert_eq!(
        resolve(Deployment::Desktop, &env),
        Decision::Silent(Silence::UnusableDsn)
    );
}

#[test]
fn opt_out_silences_the_baked_dsn() {
    let env = DesktopEnv::with_baked(MapEnv::new([(ENABLE_ENV, "off")]), Some(BAKED));
    assert_eq!(
        resolve(Deployment::Desktop, &env),
        Decision::Silent(Silence::OptedOut)
    );
}

#[test]
fn without_a_baked_dsn_nothing_changes() {
    let env = DesktopEnv::with_baked(MapEnv::new(Vec::<(&str, &str)>::new()), None);
    assert_eq!(
        resolve(Deployment::Desktop, &env),
        Decision::Silent(Silence::NoDsn)
    );
}

#[test]
fn other_keys_pass_through() {
    let env = DesktopEnv::with_baked(MapEnv::new([("OTHER", "x")]), Some(BAKED));
    assert_eq!(env.get("OTHER").as_deref(), Some("x"));
    assert_eq!(env.get("MISSING"), None);
}

#[test]
fn sentry_test_argument_is_recognised_exactly() {
    let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(sentry_test_args(args(&["app"])), None);
    assert_eq!(sentry_test_args(args(&["app", "-psn_0_12345"])), None);
    assert_eq!(sentry_test_args(args(&["app", "sentry-test"])), Some(None));
    assert_eq!(
        sentry_test_args(args(&["app", "sentry-test", "--message", "hi"])),
        Some(Some("hi".to_string()))
    );
    assert_eq!(
        sentry_test_args(args(&["app", "sentry-test", "--message=yo"])),
        Some(Some("yo".to_string()))
    );
}

/// Every desktop build carries the desktop project's DSN, and it is one the
/// core accepts.
#[test]
fn the_desktop_dsn_is_compiled_in_and_usable() {
    assert_eq!(baked_dsn(), Some(DESKTOP_DSN));
    let env = DesktopEnv::new(MapEnv::new(Vec::<(&str, &str)>::new()));
    assert_eq!(
        destination(&resolve(Deployment::Desktop, &env)).as_deref(),
        Some(DESKTOP_DSN)
    );
}
