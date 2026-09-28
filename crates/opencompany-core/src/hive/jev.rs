//! Jev routing over the TinyHumans System One proxy.
//!
//! A desk episode asks Jev — TypeSafe's System One model — who should speak
//! next: which seats a message needs, whether a broadcast fans out, whether the
//! question is too ambiguous to route at all. `tinyhivemind_typesafe::JevRouter`
//! owns that conversation; it deliberately owns **no HTTP client, credential
//! or runtime**, and asks the host for exactly one thing — a
//! [`SystemOneTransport`] that can put a [`SystemOneRequest`] on the wire and
//! bring a [`SystemOneResponse`] back. This module is that thing.
//!
//! The wire goes to **TinyHumans**, not to TypeSafe directly. The
//! `/agent-integrations/openrouter/systemone` route on the TinyHumans API is a
//! System One proxy: same request body, same response body, but authenticated
//! with the TinyHumans key every hosted and desktop instance already has, so a
//! company needs no second credential to route. The proxy resolves the
//! `jev-latest` alias to a concrete `typesafe/jev-*` id and reports that id in
//! the response's `model`; the router records it and never compares it to the
//! request's, so the alias is safe to keep asking for.
//!
//! Three rules, each the reason for a line below:
//!
//! * **No key, no router.** [`jev_router`] answers `None` when no TinyHumans
//!   credential resolves, and the caller routes by desk lead and explicit
//!   mention instead (`docs/spec/runtime/hive.md`). A router that would fail
//!   every call is worse than none: every episode would pay a timeout before
//!   falling back.
//! * **The key resolves the way every other managed call's key does** —
//!   `OPENCOMPANY_INFERENCE_KEY`, then the TinyHumans token tiers (projected
//!   file over static `TINYHUMANS_API_KEY`) — through the same
//!   [`Credential`] seam the harness provider uses, so a rotated token file is
//!   picked up per request and a 401 invalidates the cached read.
//! * **The URL is the operator's to move, not to downgrade.**
//!   `OPENCOMPANY_JEV_URL` points a staging box at its own proxy; it must be
//!   `https`, or `http` to a loopback host, for the same reason the analytics
//!   collector has that rule: the credential is a request header on every
//!   call, and a plain-`http` endpoint on a container network would put it on
//!   the wire in the clear.
//!
//! One retry, then the caller's fallback. A `429`/`529` (TypeSafe's documented
//! transient statuses, `classify_retry`) or any `5xx` from the proxy in front
//! gets exactly one immediate second attempt; anything else — a `4xx`, a
//! timeout, a connection refused — is returned at once as
//! [`Error::Transport`] and the driver routes the round without Jev. Routing
//! is on the critical path of every desk round, so a long backoff here would
//! be a long silence in the room.

use std::time::Duration;

use tinyhivemind_typesafe::{
    Error, JevRouter, RetryClass, SystemOneRequest, SystemOneResponse, SystemOneTransport,
    SystemOneTransportFuture, classify_retry,
};

use crate::app::config::EnvSource;
use crate::company::credentials::Credential;
use crate::error::{OpenCompanyError, Result};

/// The TinyHumans System One proxy every instance routes through unless
/// [`JEV_URL_ENV`] says otherwise.
pub const DEFAULT_JEV_URL: &str =
    "https://api.tinyhumans.ai/agent-integrations/openrouter/systemone";

/// The environment variable that moves the proxy: `https`, or `http` to a
/// loopback host. Anything else is refused at construction, not at first use.
pub const JEV_URL_ENV: &str = "OPENCOMPANY_JEV_URL";

/// One routing call's ceiling, connect included. A round waits on this before
/// it can start its seats, so it is short: a routing model that has not
/// answered in thirty seconds is one the driver should stop waiting for.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The host-owned [`SystemOneTransport`]: one `reqwest` client, one proxy URL,
/// one credential. Cheap to clone; the client is a connection pool behind an
/// `Arc`.
#[derive(Clone)]
pub struct TinyHumansSystemOne {
    client: reqwest::Client,
    url: String,
    bearer: Credential,
}

impl std::fmt::Debug for TinyHumansSystemOne {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The credential never prints; its tier is the only thing worth seeing.
        f.debug_struct("TinyHumansSystemOne")
            .field("url", &self.url)
            .field("bearer", &self.bearer.source())
            .finish()
    }
}

impl TinyHumansSystemOne {
    /// A transport to `url` presenting `bearer`. Refuses a URL the credential
    /// may not cross (see [`validate_url`]) and a client that cannot be built.
    pub fn new(url: impl Into<String>, bearer: Credential) -> Result<Self> {
        let url = validate_url(url.into())?;
        let client = client_builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| OpenCompanyError::Config(format!("jev http client: {error}")))?;
        Ok(Self {
            client,
            url,
            bearer,
        })
    }

    /// Convenience over [`Self::new`] for a key already in hand.
    pub fn with_key(url: impl Into<String>, key: impl Into<String>) -> Result<Self> {
        Self::new(url, Credential::from_value(key))
    }

    /// The proxy this transport posts to.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// One attempt: post the request, map every failure onto
    /// [`Error::Transport`] with the status when there is one.
    async fn attempt(
        &self,
        request: &SystemOneRequest,
    ) -> std::result::Result<SystemOneResponse, Error> {
        let mut builder = self.client.post(&self.url).json(request);
        let bearer = self
            .bearer
            .current()
            .await
            .map_err(|error| Error::Transport {
                status: None,
                message: format!("jev credential: {error}"),
            })?;
        if let Some(token) = bearer {
            builder = builder.bearer_auth(token);
        }
        let response = builder.send().await.map_err(|error| Error::Transport {
            status: error.status().map(|status| status.as_u16()),
            message: describe_reqwest(&error),
        })?;
        let status = response.status();
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                // A projected token file may have rotated under us; the next
                // read comes from disk again. A static key is unaffected.
                self.bearer.invalidate();
            }
            let message = response.text().await.unwrap_or_default();
            return Err(Error::Transport {
                status: Some(status.as_u16()),
                message: truncate(message),
            });
        }
        response
            .json::<SystemOneResponse>()
            .await
            .map_err(|error| Error::Transport {
                status: Some(status.as_u16()),
                message: format!("undecodable System One response: {error}"),
            })
    }
}

impl SystemOneTransport for TinyHumansSystemOne {
    fn evaluate<'a>(&'a self, request: &'a SystemOneRequest) -> SystemOneTransportFuture<'a> {
        Box::pin(async move {
            match self.attempt(request).await {
                Err(Error::Transport {
                    status: Some(status),
                    message,
                }) if should_retry(status) => {
                    tracing::debug!(status, %message, url = %self.url, "jev transport: retrying once");
                    self.attempt(request).await
                }
                outcome => outcome,
            }
        })
    }
}

/// The Jev router for this process, or `None` when nothing can authenticate
/// to the proxy.
///
/// `key` is a credential the caller already holds — a company's own key from
/// its secret store — and outranks the environment. With `None`, the key
/// resolves the way the harness resolves the managed-inference credential:
/// `OPENCOMPANY_INFERENCE_KEY`, then the TinyHumans token file, then
/// `TINYHUMANS_API_KEY`. The URL is [`JEV_URL_ENV`] or [`DEFAULT_JEV_URL`].
///
/// `Ok(None)` is the ordinary offline answer and the caller logs it once and
/// routes by lead and mention. `Err` is a configuration the operator has to
/// fix — an `http://` proxy on a routable host — and is never silently
/// downgraded to `None`, because "routing without Jev" would then look like
/// the intended state rather than a misconfiguration.
pub fn jev_router(
    env: &dyn EnvSource,
    key: Option<&str>,
) -> Result<Option<JevRouter<TinyHumansSystemOne>>> {
    let url = env
        .get(JEV_URL_ENV)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_JEV_URL.to_string());
    let credential = match key.map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) => Credential::from_value(key),
        None => crate::harness::built_in::provider::hosted_endpoint_from_env_at(env, None)
            .map(|(credential, _url)| credential)
            .unwrap_or_default(),
    };
    if !credential.configured() {
        return Ok(None);
    }
    let transport = TinyHumansSystemOne::new(url, credential)?;
    Ok(Some(JevRouter::new(transport)))
}

/// Whether a failed status earns the one retry: TypeSafe's documented
/// transient statuses per [`classify_retry`], plus any `5xx` — the proxy in
/// front of TypeSafe answers `502`/`503`/`504` for its own upstream hiccups,
/// which are exactly as transient as a `529` and just as worth one more try.
pub fn should_retry(status: u16) -> bool {
    matches!(classify_retry(status), RetryClass::Retryable) || (500..=599).contains(&status)
}

/// The URL rule: parseable, `http(s)`, with a host, and — since the credential
/// rides in a header on every call — `https`, or `http` only to a loopback
/// host. Mirrors the analytics collector rule and reuses its judgement.
pub fn validate_url(url: String) -> Result<String> {
    let parsed = url::Url::parse(&url)
        .map_err(|error| OpenCompanyError::Config(format!("{JEV_URL_ENV}: {error}")))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none_or(str::is_empty) {
        return Err(OpenCompanyError::Config(format!(
            "{JEV_URL_ENV} must be an http(s) URL with a host, got {url:?}"
        )));
    }
    if !crate::analytics::config::is_secure_endpoint(&url) {
        return Err(OpenCompanyError::Config(format!(
            "{JEV_URL_ENV} is a plain http:// URL to a non-loopback host; the TinyHumans key \
             is a request header on every routing call and would cross the network in the \
             clear. Use https, or a loopback address."
        )));
    }
    Ok(url)
}

/// The TLS-aware client builder the embedded runtime's own backend calls use,
/// so a corporate CA or proxy configured for OpenHuman applies here too.
fn client_builder() -> reqwest::ClientBuilder {
    openhuman_core::util::tls::tls_client_builder()
}

fn describe_reqwest(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        format!("timed out after {}s", REQUEST_TIMEOUT.as_secs())
    } else if error.is_connect() {
        "connection failed".to_string()
    } else {
        // `reqwest::Error`'s Display includes the URL; that is fine here (it
        // holds no secret — the bearer is a header) and names the proxy.
        error.to_string()
    }
}

/// Keeps a provider error body to something a log line can hold.
fn truncate(message: String) -> String {
    const MAX: usize = 512;
    if message.len() <= MAX {
        return message;
    }
    let mut cut = MAX;
    while !message.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &message[..cut])
}

#[cfg(test)]
#[path = "jev_tests.rs"]
mod tests;
