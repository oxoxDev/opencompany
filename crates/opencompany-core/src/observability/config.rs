//! The enable/disable decision: whether this process reports crashes at all,
//! where to, and under which release.
//!
//! Kept apart from the client for the reason `analytics::config` is: it is the
//! part that has to be *provably* right, and it can be. It is pure — an
//! [`EnvSource`] and a [`Deployment`] in, a [`Decision`] out — so every branch
//! of it is exercised by the default `cargo test`, with no network, no
//! `sentry::` type in scope, and no `crash-reporting` feature.

use crate::app::config::EnvSource;
use crate::app::deployment::Deployment;

/// The Sentry DSN. Configuration that always wins over
/// [`DEFAULT_HOSTED_TENANT_DSN`].
pub const DSN_ENV: &str = "OPENCOMPANY_SENTRY_DSN";

/// The TinyHumans `opencompany-core` project's DSN, used **only** by a
/// [`Deployment::HostedTenant`] whose `OPENCOMPANY_SENTRY_DSN` is unset or
/// blank.
///
/// Compiled in so the hosted-tenant image reports without anything injecting
/// it. It is gated to the hosted-tenant deployment, not merely to the
/// `crash-reporting` feature, because the feature is *not* tenant-only: the
/// desktop app compiles it (and has its own project and DSN, see
/// `opencompany-app/src/crash.rs`), and a self-hoster can compile it into
/// `deploy/Dockerfile` through `OPENCOMPANY_FEATURES`. Neither of those may
/// report to TinyHumans' project on a default nobody chose, so for them an
/// absent DSN is still [`Silence::NoDsn`]. A DSN's public key only authorizes
/// writes to this one project, and it already ships in the tenant image.
pub const DEFAULT_HOSTED_TENANT_DSN: &str =
    "https://ef2ebd38c55bc102fd7fd06a1a6eb867@sentry.tinyhumans.ai/10";

/// Operator override: `off` forbids reporting and outranks everything else.
///
/// A DSN is already the switch — unset means silence — so this exists for the
/// case where the DSN is injected by something the operator does not edit (a
/// container manager, a shared `.env`) and they want it off anyway. `on` is
/// accepted and means nothing beyond "not off": there is nothing to force,
/// because without a DSN there is nowhere to send.
pub const ENABLE_ENV: &str = "OPENCOMPANY_SENTRY";

/// Overrides the `environment` tag. Defaults to the deployment kind.
pub const ENVIRONMENT_ENV: &str = "OPENCOMPANY_SENTRY_ENVIRONMENT";

/// The fraction of requests recorded as performance transactions, `0.0` to
/// `1.0`. **Absent means `0.0`**, and that is the point.
///
/// Errors and transactions are billed separately by Sentry, and a transaction
/// is emitted for every request rather than only when something goes wrong — so
/// a rate this repository chose on an operator's behalf would be a recurring
/// bill they did not ask for. The same argument [`DSN_ENV`] makes about whose
/// quota this is, one level down: having decided to report at all is not the
/// same as having decided to report *every request*.
///
/// It is also a much larger content surface than an error. A transaction
/// carries a span per outbound request, each with a URL — which is why
/// [`super::sanitize_transaction`] exists and why turning this on without it
/// would undo the care in `observability::redaction`.
pub const TRACES_SAMPLE_RATE_ENV: &str = "OPENCOMPANY_SENTRY_TRACES_SAMPLE_RATE";

/// A Sentry DSN.
///
/// A newtype rather than a bare `String`, on the
/// `analytics::ClientCredentials` precedent and for the same reason: it must never be printed, logged or
/// serialized by accident. It derives **neither** `Debug` nor `Serialize` — the
/// hand-written `Debug` redacts — because `serde_json::to_value(&some_config)`
/// is exactly how a credential reaches a payload.
///
/// A DSN's public key is not a password (it ships in every browser bundle that
/// reports to the same project), but it is not nothing either: it authorizes
/// writes to somebody's quota, and treating it as printable is how it ends up
/// in a screenshot of a boot log. [`Dsn::loggable`] is the only shape that is
/// allowed out.
#[derive(Clone, PartialEq, Eq)]
pub struct Dsn(String);

impl Dsn {
    /// Wraps a DSN that [`parse_dsn`] has already accepted.
    fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The DSN, for the one caller that hands it to the client.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// `scheme://host/path` — the destination, with the public key removed.
    ///
    /// This is what a boot line is allowed to name. Naming nothing would be
    /// worse than naming a redacted form: "reporting" with no destination is
    /// unactionable when an operator has two projects and events are landing in
    /// the wrong one.
    pub fn loggable(&self) -> String {
        match url::Url::parse(&self.0) {
            Ok(mut url) => {
                // Both halves, in the order `analytics::boot::loggable_endpoint`
                // learned to do it: userinfo carries the public key here, and a
                // query string is where anyone fronting an ingest with their own
                // proxy puts a key of their own.
                let _ = url.set_username("");
                let _ = url.set_password(None);
                url.set_query(None);
                url.set_fragment(None);
                url.to_string()
            }
            // Unreachable: `parse_dsn` built this through the same parser. A
            // constant rather than the raw value, because the one thing this
            // function may never do is fall back to printing the DSN.
            Err(_) => "<unprintable dsn>".to_string(),
        }
    }
}

impl std::fmt::Debug for Dsn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Dsn(<redacted>)")
    }
}

/// Why a process is not reporting. Printed once at boot, so an operator who
/// *expected* crash reports can tell "switched off" from "misconfigured".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Silence {
    /// The operator set `OPENCOMPANY_SENTRY=off`.
    OptedOut,
    /// No DSN is configured and this is not a hosted tenant. **The default**
    /// for desktop, self-hosted and CI runs.
    NoDsn,
    /// `OPENCOMPANY_SENTRY` was set to something this does not recognise.
    ///
    /// A separate reason from [`Self::OptedOut`] on purpose, on the lesson
    /// `analytics::config::Silence::Unreadable` records: an operator who typed
    /// `of` gets the outcome they meant *and* a line saying their value was not
    /// understood, rather than silence they cannot tell from a working opt-out.
    Unreadable,
    /// `OPENCOMPANY_SENTRY_DSN` is set to something that is not a Sentry DSN —
    /// no scheme, a scheme that is not `http`/`https`, no public key, no host,
    /// no project id, or bytes this process cannot read.
    ///
    /// Silence rather than a client that cannot send, because the failure this
    /// prevents is the one analytics was rebuilt to prevent: boot prints
    /// "reporting to …", a client is installed, and every envelope dies inside
    /// the transport behind a log line nobody has enabled. The operator reading
    /// their own logs has no reason to look again.
    ///
    /// The reason never quotes the value — see [`Dsn`].
    UnusableDsn,
    /// A DSN was configured and accepted, but this binary was compiled without
    /// the `crash-reporting` feature, so there is no client in it to install.
    ///
    /// The line reports what the process will **do**, not what was configured.
    /// Saying "reporting to …" here would be the exact opposite of the truth.
    NotCompiled,
}

impl Silence {
    /// The stable reason slug, for the boot line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OptedOut => "operator opted out",
            Self::NoDsn => "no DSN is configured",
            Self::Unreadable => "the OPENCOMPANY_SENTRY value is not recognised",
            Self::UnusableDsn => "the OPENCOMPANY_SENTRY_DSN value is not a usable Sentry DSN",
            Self::NotCompiled => {
                "a DSN was configured, but this build was compiled without the `crash-reporting` feature"
            }
        }
    }
}

/// What this process will do about **performance tracing**, which is a
/// separate decision from whether it reports errors.
///
/// Separate because the costs are different in kind. An error event is rare and
/// is the thing an operator asked for; a transaction is emitted for every
/// served request whether or not anything went wrong, is billed on its own
/// quota, and carries a span — with a URL — for every outbound call the request
/// made. An operator who wants crash reports has not thereby asked for a
/// per-request feed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Traces {
    /// Record no transactions. **The default**, and what every install that
    /// sets only a DSN gets.
    Off,
    /// Record this fraction of requests. Always in `0.0 < rate <= 1.0`.
    Sampled(f32),
    /// [`TRACES_SAMPLE_RATE_ENV`] was set to something that is not a fraction
    /// between 0 and 1.
    ///
    /// A distinct state rather than a silent fall back to [`Self::Off`], on the
    /// lesson [`Silence::Unreadable`] records: an operator who typed `0,5` or
    /// `50%` gets the safe outcome *and* a line saying their value was not
    /// understood, instead of silence they cannot tell from a working default.
    Unreadable,
}

impl Traces {
    /// The rate to hand the client. Zero unless a rate was configured and read.
    pub fn rate(self) -> f32 {
        match self {
            Self::Sampled(rate) => rate,
            Self::Off | Self::Unreadable => 0.0,
        }
    }

    /// Whether any transaction will be recorded.
    pub fn is_on(self) -> bool {
        self.rate() > 0.0
    }

    /// The clause the boot line adds after the destination.
    fn describe(self) -> String {
        match self {
            Self::Off => "performance tracing off".to_string(),
            Self::Sampled(rate) => format!("tracing {}% of requests", rate * 100.0),
            Self::Unreadable => format!(
                "performance tracing off ({TRACES_SAMPLE_RATE_ENV} is not a number between 0 and 1)"
            ),
        }
    }
}

/// What this process will do about crash reporting.
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// Report nothing. No client is constructed, so nothing *can* be reported.
    Silent(Silence),
    /// Report to `dsn`, tagged with `environment` and `release`.
    Report {
        /// The operator's ingest endpoint.
        dsn: Dsn,
        /// The `environment` tag: the deployment kind unless overridden.
        environment: String,
        /// The `release` tag: `opencompany@<version>+<commit>`.
        release: String,
        /// Whether, and how much, to record performance transactions.
        traces: Traces,
    },
}

impl Decision {
    /// One line naming what the process will do and, when it will do nothing,
    /// why. Mirrors `analytics::boot::describe` — a `println!` rather than a
    /// `tracing::info!` for the same reason it is there: the CLI's default
    /// `EnvFilter` is `error`, so an `info!` would be exactly as silent as the
    /// misconfiguration it reports.
    pub fn describe(&self) -> String {
        match self {
            Self::Silent(reason) => format!("crash reporting: off ({})", reason.as_str()),
            Self::Report {
                dsn,
                environment,
                release,
                traces,
            } => format!(
                "crash reporting: reporting to {} as {release} ({environment}), {}",
                dsn.loggable(),
                traces.describe()
            ),
        }
    }
}

/// The canonical release tag: `opencompany@<version>[+<commit>]`.
///
/// Both halves are needed. [`crate::VERSION`] has read `0.1.0` for thousands of
/// commits, so a release built from it alone cannot tell two builds apart —
/// which is the whole question a stack trace raises. [`crate::BUILD_COMMIT`]
/// already resolves an explicit `OPENCOMPANY_BUILD_COMMIT`, then `git`, then
/// `GITHUB_SHA`, then the literal `"unknown"` (`src/build_stamp.rs`), so no
/// second environment variable is invented here for something the build already
/// stamps.
///
/// The `"unknown"` case is dropped rather than appended: `opencompany@0.1.0` is
/// an honest "this build cannot say which commit it is", and
/// `opencompany@0.1.0+unknown` is a release name that looks like a commit and
/// is not one. A `-dirty` suffix is kept — a build from a modified tree is a
/// different build, and the tag should say so.
pub fn release_tag() -> String {
    release_tag_from(crate::VERSION, crate::BUILD_COMMIT)
}

/// [`release_tag`] with its inputs supplied, so the `unknown` and `-dirty`
/// branches are reachable from a test rather than only from a build.
fn release_tag_from(version: &str, commit: &str) -> String {
    let commit = commit.trim();
    if commit.is_empty() || commit == "unknown" {
        format!("opencompany@{version}")
    } else {
        format!("opencompany@{version}+{commit}")
    }
}

/// Parses a candidate DSN, or `None` when it is not one.
///
/// Validated with `url`, the same parser the transport will hand it to, on the
/// rule issue #673 settled for a different call site: a second, hand-rolled
/// reader of a URL grammar is a bypass waiting to be found. On top of the parse
/// this asserts the four things that make a URL a *Sentry* DSN, because a URL
/// that parses and is not a DSN is precisely the input that resolves to
/// "reporting" and then never delivers:
///
/// 1. an `http`/`https` scheme — nothing else can be posted to;
/// 2. a non-empty username, which is the public key;
/// 3. a host;
/// 4. a path whose last segment is a project id.
///
/// A **password** is refused rather than stripped. The `https://key:secret@…`
/// form is a DSN from before 2016 whose secret half is no longer accepted by
/// any ingest, so a DSN carrying one is either a stale copy — silence with a
/// reason beats a client that 401s forever — or an operator who pasted a
/// credential into the wrong variable, which is worth refusing loudly.
fn parse_dsn(raw: &str) -> Option<Dsn> {
    let raw = raw.trim();
    let url = url::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    if url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if url.host_str().is_none_or(str::is_empty) {
        return None;
    }
    let project = url.path().rsplit('/').next().unwrap_or_default();
    if project.is_empty() {
        return None;
    }
    Some(Dsn::new(raw))
}

/// [`parse_dsn`], reachable from the gated tests in [`super`].
///
/// Those tests pin this grammar against the SDK's own parser
/// (`the_two_dsn_parsers_agree`), which needs to call it from a module that
/// has `sentry::` in scope — and this module deliberately does not.
//
// Gated on the feature as well as on `cfg(test)`: the only caller is the
// `the_two_dsn_parsers_agree` test, which needs `sentry::` in scope, so in a
// default build this would be an unused function and `-D dead-code` is on.
#[cfg(all(test, feature = "crash-reporting"))]
pub(crate) fn parse_dsn_for_test(raw: &str) -> Option<Dsn> {
    parse_dsn(raw)
}

/// Resolves this process's crash-reporting decision from the environment.
///
/// The order is the order of the switches' authority, and only the first
/// applicable one is consulted:
///
/// 1. `OPENCOMPANY_SENTRY=off` — silence, whatever else is set;
/// 2. an `OPENCOMPANY_SENTRY` value that is neither `on` nor `off` — silence,
///    naming the value as unreadable rather than guessing at it;
/// 3. no DSN — [`DEFAULT_HOSTED_TENANT_DSN`] for a hosted tenant, silence for
///    every other deployment (desktop and self-hosted start here);
/// 4. a DSN that is not usable — silence, naming that;
/// 5. otherwise, report.
///
/// Unlike `analytics::config::resolve`, the deployment kind gates only the
/// compiled-in default DSN; otherwise it names the `environment` tag. Analytics reports to a
/// collector *this* project runs, so who is allowed to report is the whole
/// question there; a crash report goes to an endpoint the operator configured
/// in their own organisation, and a self-hoster who sets a DSN has asked for
/// exactly one thing and should get it.
pub fn resolve(deployment: Deployment, env: &dyn EnvSource) -> Decision {
    // `get_os`, not `get`, on the lesson `Deployment::from_env` records: `get`
    // maps a non-Unicode value to `None`, which here would read as "nobody set
    // the switch" and fall through to reporting — telemetry turned ON by a
    // malformed variable, on the one switch that exists to turn it off. A
    // BLANK value is still absent, so a launcher that exports an empty
    // variable changes nothing.
    match env.get_os(ENABLE_ENV) {
        None => {}
        Some(raw) => match raw.to_str().map(|value| value.trim().to_ascii_lowercase()) {
            Some(value) if value.is_empty() => {}
            Some(value) if value == "off" => return Decision::Silent(Silence::OptedOut),
            Some(value) if value == "on" => {}
            _ => return Decision::Silent(Silence::Unreadable),
        },
    }

    // An unset or blank DSN falls back to the compiled-in default for a hosted
    // tenant only — see [`DEFAULT_HOSTED_TENANT_DSN`] for why nothing else gets
    // it. A DSN that is set but unreadable or malformed is the operator's and
    // is reported, never papered over by the default.
    let fallback = || match deployment {
        Deployment::HostedTenant => parse_dsn(DEFAULT_HOSTED_TENANT_DSN)
            .map_or(Decision::Silent(Silence::UnusableDsn), |dsn| {
                report(dsn, deployment, env)
            }),
        _ => Decision::Silent(Silence::NoDsn),
    };
    let Some(raw) = env.get_os(DSN_ENV) else {
        return fallback();
    };
    let Some(raw) = raw.to_str() else {
        // Bytes this process cannot read are a *misconfigured* DSN, not an
        // absent one, and the two want different lines.
        return Decision::Silent(Silence::UnusableDsn);
    };
    if raw.trim().is_empty() {
        return fallback();
    }
    let Some(dsn) = parse_dsn(raw) else {
        return Decision::Silent(Silence::UnusableDsn);
    };
    report(dsn, deployment, env)
}

/// The reporting decision for an accepted DSN.
fn report(dsn: Dsn, deployment: Deployment, env: &dyn EnvSource) -> Decision {
    Decision::Report {
        dsn,
        environment: environment(deployment, env),
        release: release_tag(),
        traces: traces(env),
    }
}

/// The performance-tracing decision.
///
/// Absent, blank or unreadable bytes mean [`Traces::Off`] — the same "a
/// variable nobody set changes nothing" rule the enable switch follows, and for
/// the stronger reason that this one costs money. A value that parses but is
/// outside `0.0..=1.0` is [`Traces::Unreadable`] rather than clamped: `100` is
/// far more likely to mean "100%" than "1.0", and silently reading it as
/// `1.0`-after-clamping would record every request for an operator who thought
/// they had asked for something else.
///
/// An explicit `0` is [`Traces::Off`] rather than `Sampled(0.0)`, so the boot
/// line reads the same as it does for an operator who set nothing — which is
/// the same thing the process will do.
fn traces(env: &dyn EnvSource) -> Traces {
    let Some(raw) = env.get_os(TRACES_SAMPLE_RATE_ENV) else {
        return Traces::Off;
    };
    let Some(raw) = raw.to_str().map(str::trim) else {
        return Traces::Unreadable;
    };
    if raw.is_empty() {
        return Traces::Off;
    }
    match raw.parse::<f32>() {
        // `is_finite` rejects `NaN` and `inf`, both of which `parse` accepts
        // and neither of which is a sample rate.
        Ok(rate) if rate.is_finite() && rate == 0.0 => Traces::Off,
        Ok(rate) if rate.is_finite() && (0.0..=1.0).contains(&rate) => Traces::Sampled(rate),
        _ => Traces::Unreadable,
    }
}

/// The `environment` tag.
///
/// Defaults to the deployment kind — `desktop`, `self-hosted`, `hosted-tenant`
/// — rather than to `production`/`development`, because that is the distinction
/// this crate already models (`src/app/deployment.rs`) and a second, parallel
/// notion of "which environment am I" would drift from it. An operator running
/// staging and production tenants overrides it per deployment.
///
/// Lower-cased and trimmed so `Production` and `production ` are one value
/// rather than three rows in a filter.
fn environment(deployment: Deployment, env: &dyn EnvSource) -> String {
    env.get(ENVIRONMENT_ENV)
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| deployment.as_str().to_string())
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
