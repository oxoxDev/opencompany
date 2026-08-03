//! Redeeming an ecosystem console handoff code for the identity behind it.
//!
//! Sign-in happens once, on the TinyHumans dashboard. Crossing from there into
//! a company's console cannot carry the ecosystem JWT: this tenant has no way
//! to verify one. The signing secret is the hub's, and handing it to every
//! tenant so each could check a signature would also let every tenant *mint* a
//! token for any user in the ecosystem — the plainest possible way to lose the
//! isolation the hosting layer exists to provide.
//!
//! So the browser carries an opaque, single-use code instead, and this module
//! trades it back to the hub for `{ email, slug }`. What the tenant learns is
//! one address and its own slug — never a credential, never anything it could
//! replay against another company.
//!
//! The [`ConsoleHandoffExchange`] trait and its offline
//! [`MockConsoleHandoffExchange`] compile in the default build, so the whole
//! route — eligibility, session minting, every refusal — is exercised without
//! linking a network crate. Only [`HttpConsoleHandoffExchange`] is gated behind
//! the existing `tinyhumans` feature, which is already what "this instance
//! talks to the hub about its credential's owner" means. It does not earn a
//! feature flag of its own.

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;

use async_trait::async_trait;

use crate::Result;

/// Who a redeemed handoff code stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandoffIdentity {
    /// The ecosystem address that signed in on the dashboard. Not normalized
    /// here — the route does that before it goes anywhere near a user lookup.
    pub email: String,
    /// The instance slug the code was minted for. The caller must compare this
    /// against its own; the hub binding it is not the same as this tenant
    /// checking it.
    pub slug: String,
}

/// The hub, scoped to console handoff redemption.
#[async_trait]
pub trait ConsoleHandoffExchange: Send + Sync {
    /// Spends `code` and returns the identity it was minted for.
    ///
    /// Redemption is destructive at the hub, so a second call with the same
    /// code fails. Implementations must not retry a failed redemption: a
    /// transport error that actually spent the code would burn a live sign-in.
    async fn redeem(&self, code: &str) -> Result<HandoffIdentity>;
}

/// An in-memory [`ConsoleHandoffExchange`] for offline tests and local demos.
///
/// Single use is modelled faithfully — [`redeem`](ConsoleHandoffExchange::redeem)
/// removes the code — because "the code was already spent" is the failure a
/// replay must produce, and a mock that let it succeed twice would make the
/// route's replay test prove nothing.
#[derive(Debug, Default)]
pub struct MockConsoleHandoffExchange {
    codes: StdMutex<HashMap<String, HandoffIdentity>>,
    redeemed: StdMutex<Vec<String>>,
}

impl MockConsoleHandoffExchange {
    /// An exchange that knows no codes; every redemption fails.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds one live code.
    pub fn with_code(self, code: &str, email: &str, slug: &str) -> Self {
        self.codes.lock().expect("mock poisoned").insert(
            code.to_string(),
            HandoffIdentity {
                email: email.to_string(),
                slug: slug.to_string(),
            },
        );
        self
    }

    /// Every code this mock was asked to redeem, spent or not.
    pub fn attempts(&self) -> Vec<String> {
        self.redeemed.lock().expect("mock poisoned").clone()
    }
}

/// The error a hub returns for a code that is missing, spent, or expired.
///
/// One shape for all three on purpose, mirroring the hub: telling "never
/// existed" apart from "already used" would let someone holding a stolen code
/// learn whether the real user beat them to it.
fn spent_or_expired() -> crate::error::OpenCompanyError {
    crate::error::OpenCompanyError::TinyHumans {
        code: "http_400".to_string(),
        message: "Invalid or expired handoff code".to_string(),
    }
}

#[async_trait]
impl ConsoleHandoffExchange for MockConsoleHandoffExchange {
    async fn redeem(&self, code: &str) -> Result<HandoffIdentity> {
        self.redeemed
            .lock()
            .expect("mock poisoned")
            .push(code.to_string());
        self.codes
            .lock()
            .expect("mock poisoned")
            .remove(code)
            .ok_or_else(spent_or_expired)
    }
}

/// The real HTTP exchange, compiled only under the `tinyhumans` feature.
#[cfg(feature = "tinyhumans")]
pub use http::HttpConsoleHandoffExchange;

#[cfg(feature = "tinyhumans")]
mod http {
    use super::{ConsoleHandoffExchange, HandoffIdentity};
    use crate::Result;
    use crate::error::OpenCompanyError;
    use async_trait::async_trait;
    use serde::Deserialize;

    /// The header the hub reads a tenant's workload-identity attestation from.
    ///
    /// Deliberately not `Authorization`: the redemption route runs with no auth
    /// middleware, and a bearer there would look like a credential the route
    /// checks when it is not.
    const ATTESTATION_HEADER: &str = "x-tenant-attestation";

    /// The hub's envelope for a redeemed code.
    #[derive(Debug, Deserialize)]
    struct ConsumeResponse {
        data: HandoffData,
    }

    #[derive(Debug, Deserialize)]
    struct HandoffData {
        email: String,
        slug: String,
    }

    /// A [`ConsoleHandoffExchange`] backed by `POST {api_url}/console-handoff/consume`.
    ///
    /// The route is top-level at the hub rather than under `/auth` or
    /// `/opencompany`, both of which are closed to machine principals — a
    /// tenant redeeming a code is a machine, so hosting the route inside either
    /// prefix would have meant carving a hole in exactly the rule that keeps a
    /// tenant off the control plane that owns it.
    pub struct HttpConsoleHandoffExchange {
        api_url: String,
        /// The projected workload-identity token file, when the platform mounts
        /// one. Read per request rather than at construction: a projected token
        /// is rotated in place, and a copy taken at boot goes stale within the
        /// hour. `None` where attestation is not configured, which is still
        /// every deployment today.
        attestation_token_file: Option<std::path::PathBuf>,
        http: reqwest::Client,
    }

    impl HttpConsoleHandoffExchange {
        /// Builds an exchange against `api_url`.
        pub fn new(api_url: impl Into<String>) -> Self {
            Self {
                // Trailing slashes would produce `//console-handoff/consume`.
                api_url: api_url.into().trim_end_matches('/').to_string(),
                attestation_token_file: None,
                http: reqwest::Client::new(),
            }
        }

        /// Presents the projected workload identity at `path` on every redemption.
        pub fn with_attestation_token_file(mut self, path: impl Into<std::path::PathBuf>) -> Self {
            self.attestation_token_file = Some(path.into());
            self
        }

        fn err(context: &str, e: impl std::fmt::Display) -> OpenCompanyError {
            OpenCompanyError::TinyHumans {
                code: context.to_string(),
                message: e.to_string(),
            }
        }

        /// The current attestation, or `None` when unmounted or unreadable.
        ///
        /// An unreadable file is not an error here. Where attestation is
        /// configured the hub refuses a redemption without one, which is the
        /// refusal that should be reported — not a local file-read failure
        /// dressed up as one.
        fn attestation(&self) -> Option<String> {
            let path = self.attestation_token_file.as_ref()?;
            let raw = std::fs::read_to_string(path).ok()?;
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
    }

    #[async_trait]
    impl ConsoleHandoffExchange for HttpConsoleHandoffExchange {
        async fn redeem(&self, code: &str) -> Result<HandoffIdentity> {
            let url = format!("{}/console-handoff/consume", self.api_url);
            let mut request = self
                .http
                .post(&url)
                .json(&serde_json::json!({ "code": code }));
            if let Some(token) = self.attestation() {
                request = request.header(ATTESTATION_HEADER, token);
            }

            let resp = request
                .send()
                .await
                .map_err(|e| Self::err("unreachable", e))?;

            let status = resp.status();
            if !status.is_success() {
                // The hub's message is safe to surface — it says "invalid or
                // expired", never which. The code itself is never echoed.
                let detail = resp.text().await.unwrap_or_default();
                return Err(Self::err(
                    &format!("http_{}", status.as_u16()),
                    truncate(&detail, 200),
                ));
            }

            let parsed: ConsumeResponse = resp.json().await.map_err(|e| Self::err("decode", e))?;
            Ok(HandoffIdentity {
                email: parsed.data.email,
                slug: parsed.data.slug,
            })
        }
    }

    /// Caps an error detail at `max` **characters**, never bytes: slicing a
    /// UTF-8 string by byte offset panics mid-codepoint, and an error path is
    /// the worst possible place to learn that.
    fn truncate(value: &str, max: usize) -> String {
        if value.chars().count() <= max {
            return value.to_string();
        }
        value.chars().take(max).collect()
    }
}
