//! Learning which ecosystem address is behind a platform token.
//!
//! Sign-in happens **here**, on the company's own console. The browser is sent
//! to the hub's OAuth start pointed back at this origin, the hub completes the
//! provider dance and redirects back carrying a platform JWT, and this module
//! turns that JWT into one address.
//!
//! ## Why this tenant does not verify the token
//!
//! It cannot. The signing secret belongs to the hub, and handing it to every
//! tenant so each could check a signature would also let every tenant *mint* a
//! token for any user in the ecosystem — the plainest possible way to lose the
//! isolation the hosting layer exists to provide.
//!
//! So the tenant does not verify the token; it *uses* it. It presents the token
//! to the hub's own `GET /auth/me`, exactly as the dashboard would. If the hub
//! answers with an identity, that **is** the proof: only the hub can say who a
//! token it signed belongs to, and a forged or expired one gets a 401 there.
//! No shared secret, no minted code, no second round trip to invent.
//!
//! ## What this costs, stated plainly
//!
//! The tenant briefly holds a hub credential belonging to the person signing
//! in — one that carries their ecosystem privileges, not merely their identity.
//! That is a real delegation of trust to the tenant, and it is strictly more
//! than a single-use, slug-bound code would have handed over. It is accepted
//! here because the console *is* the company: a person signing in to their own
//! company's host is already trusting that host with everything the company
//! holds. What this module owes in return is discipline — the token is used
//! once, for one request, and is never persisted, never logged, and never
//! echoed back in an error.
//!
//! Authorization is emphatically **not** delegated. The hub says who they are;
//! this company's own roster says whether they may in — the same
//! `eligibility` → `upsert_from_eligibility` → `mint_session` path a magic link
//! answers to.
//!
//! ## Shape
//!
//! The [`HubIdentityExchange`] trait and its offline [`MockHubIdentityExchange`]
//! compile in the default build, so the whole route — eligibility, session
//! minting, every refusal — is exercised without linking a network crate. Only
//! [`HttpHubIdentityExchange`] is gated behind the existing `tinyhumans`
//! feature, which is already what "this instance talks to the hub about its
//! credential's owner" means. It does not earn a feature flag of its own.

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;

use async_trait::async_trait;

use crate::Result;

/// An identity provider the hub can complete a sign-in with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HubProvider {
    /// The hub's provider slug, as it appears in `GET /auth/{id}/login`.
    pub id: &'static str,
    /// What the console calls it on the button.
    pub label: &'static str,
}

/// The providers offered on the console's sign-in screen, in OpenHuman's order.
///
/// Deliberately the same three, in the same sequence, as OpenHuman's welcome
/// screen. Someone who signs in to the desktop app with GitHub should not have
/// to hunt for it here, and an ecosystem that offers a different identity set
/// per surface teaches people that the account is per-surface too.
///
/// Discord is registered at the hub and supports login, but OpenHuman hides it
/// on welcome (`showOnWelcome: false`), so it stays hidden here too.
pub const HUB_PROVIDERS: &[HubProvider] = &[
    HubProvider {
        id: "google",
        label: "Google",
    },
    HubProvider {
        id: "github",
        label: "GitHub",
    },
    HubProvider {
        id: "twitter",
        label: "X",
    },
];

/// Builds the hub URL that starts a sign-in and comes back to `redirect_uri`.
///
/// `redirect_uri` is round-tripped by the hub verbatim, with `token=…&key=auth`
/// appended, so it must arrive percent-encoded as a single query value —
/// unescaped it would end at the console origin's own `?` and the hub would
/// read the console's `company=` as one of its own parameters.
///
/// ## Which origins the hub accepts
///
/// The hub decides, and it is the only party that can: `isAllowedFrontendRedirectUri`
/// admits a loopback `http://` URI **or** an origin that resolves to a
/// provisioned tenant in its own registry (`<slug>.<base-domain>`, or a
/// verified custom domain). A registry lookup is not something this crate can
/// mirror, and a de-provisioned tenant stops being accepted there with no
/// redeploy here.
///
/// So this builds the URL and lets the hub answer. The origin comes from
/// [`AppConfig::host_base_url`](crate::AppConfig::host_base_url), which means a
/// hosted console is `OPENCOMPANY_PUBLIC_URL=https://…` and no code change.
///
/// This once carried a local `hub_accepts_redirect_uri` copy of the hub's
/// then-loopback-only rule, so a console would not render a button that could
/// only 400 (issue #512). `tinyhumansai/backend#1243` has since landed and the
/// copy went with it — it had become the thing hiding the buttons on every
/// hosted console, which is the failure it existed to prevent, one level up.
pub fn login_start_url(api_url: &str, provider: &str, redirect_uri: &str) -> String {
    format!(
        "{}/auth/{}/login?redirectUri={}",
        api_url.trim_end_matches('/'),
        provider,
        percent_encode(redirect_uri),
    )
}

/// Builds the hub URL that starts a **key grant** and comes back to `callback_url`.
///
/// The sign-in flow above proves who someone is. This one asks the hub to mint
/// this company a key, and it is deliberately a different exchange rather than a
/// reuse of the sign-in token.
///
/// The difference is what the tenant ends up holding. A sign-in hands this
/// tenant a platform JWT carrying the person's whole ecosystem account, used for
/// one request and dropped ([`HubIdentityExchange::identify`]). A key grant
/// hands it a one-time code that redeems to exactly one scoped API key, and the
/// secret that unlocks the code (`verifier`) never leaves this host — only its
/// SHA-256 goes out, as `challenge`. So a code captured anywhere along the
/// browser's path — history, a `Referer`, a shoulder — redeems nothing.
///
/// Shaped after OpenRouter's PKCE key exchange, which solves the same problem:
/// give an application a key without a human copying one between two sites.
pub fn key_grant_url(api_url: &str, callback_url: &str, challenge: &str, name: &str) -> String {
    format!(
        "{}/auth/key?{}",
        api_url.trim_end_matches('/'),
        key_grant_query(callback_url, challenge, name),
    )
}

/// The scopes a key grant asks the hub to put on the key it mints.
///
/// Only `connections` turns on anything the ask decides. Every grant carries
/// the set a human could have minted by hand whether it asks or not, and the
/// hub drops a name outside its grantable vocabulary rather than refusing the
/// grant — so this stays the one scope a console on this side would otherwise
/// never be handed, and the list is not a place to collect scopes nothing here
/// spends.
///
/// `connections` is what `/agent-integrations/composio/*` enforces. Without it
/// a key minted to a console that is not a provisioned tenant — a desktop
/// install, a developer's laptop — reaches managed Composio and is told it is
/// missing the scope, which is the whole of the third-party integrations
/// surface answering 403.
pub const KEY_GRANT_SCOPES: &[&str] = &["connections"];

/// The grant parameters as one query string, without the endpoint.
///
/// Split out because the same parameters are read by two pages: the API's
/// `GET /auth/key`, which acts on them, and the site's `/connect`, which shows
/// a person who is asking and lets them pick a provider before handing off to
/// exactly that endpoint ([`hub_account::connect_url`](crate::server::hub_account::connect_url)).
/// Building them once means the challenge cannot differ between the two — and
/// the same holds for [`KEY_GRANT_SCOPES`], which decides what the key may do
/// and is named on the consent screen the approver reads. A scope asked for on
/// one path and not the other would mint a key whose reach depended on which
/// page the browser happened to go through.
pub fn key_grant_query(callback_url: &str, challenge: &str, name: &str) -> String {
    format!(
        "callback_url={}&code_challenge={}&code_challenge_method=S256&name={}&scopes={}",
        percent_encode(callback_url),
        percent_encode(challenge),
        percent_encode(name),
        percent_encode(&KEY_GRANT_SCOPES.join(",")),
    )
}

/// Percent-encodes `value` for use as a single query-string value.
///
/// Hand-rolled rather than pulled in: the crate has no direct URL dependency in
/// the default build, and adding one to escape a handful of characters would
/// move `Cargo.lock` for no benefit. Keeps only the RFC 3986 unreserved set,
/// which is stricter than necessary and therefore cannot under-escape.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Who a platform token stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubIdentity {
    /// The ecosystem address the hub resolved the token to. Not normalized
    /// here — the route does that before it goes anywhere near a user lookup.
    pub email: String,
}

/// The hub, scoped to the one question this tenant may ask it: whose token is
/// this?
#[async_trait]
pub trait HubIdentityExchange: Send + Sync {
    /// Resolves `token` to the address that holds it.
    ///
    /// Implementations must treat `token` as a live credential: never log it,
    /// never store it, and never include it in an error. It is the caller's
    /// only proof of identity and would be replayable by anyone who read it.
    async fn identify(&self, token: &str) -> Result<HubIdentity>;

    /// Trades a one-time grant `code` and its `verifier` for a TinyHumans key.
    ///
    /// The other half of [`key_grant_url`]. Returns the plaintext key, which the
    /// hub emits exactly once and cannot reissue — so a caller that drops it has
    /// to send the person through the flow again, and must store it before doing
    /// anything else that can fail.
    ///
    /// Implementations must treat both arguments and the returned key as live
    /// credentials: never log them, never echo them into an error.
    async fn redeem_key_grant(&self, code: &str, verifier: &str) -> Result<String>;

    /// Reads the billing standing of the account a **key** belongs to.
    ///
    /// The one call in this trait that presents the company's own credential
    /// rather than a person's: it answers "how much is left, and on what plan",
    /// which is a property of the account the key spends from. It is a read and
    /// nothing else — topping up and changing a plan move money and stay on the
    /// hub's dashboard behind that person's own sign-in, which is why this has
    /// no counterpart that writes.
    ///
    /// Implementations must treat `key` as a live credential: never log it,
    /// never echo it into an error.
    async fn billing_summary(&self, key: &str) -> Result<BillingSummary>;
}

/// What an account's money is doing, as the console renders it.
///
/// A flattened copy of the hub's `GET /payments/summary` rather than a passthrough
/// of its JSON: the console is a different product on a different release
/// cadence, and a shape it merely forwards is one that changes under it without
/// anybody choosing to. Every field here is one the card actually draws.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingSummary {
    /// Everything spendable, promotional credit and top-up together, in USD.
    pub balance_usd: f64,
    /// The plan slug the account is on (`free`, `pro`, …).
    pub plan: String,
    /// Whether a paid subscription is live right now.
    pub active_subscription: bool,
    /// When the current plan lapses, as the hub stated it. `None` on a plan
    /// that does not expire.
    pub plan_expiry: Option<String>,
    /// Where a person tops the account up, on the hub that issued the key.
    pub top_up_url: Option<String>,
    /// Where a person changes the plan.
    pub manage_url: Option<String>,
}

/// An in-memory [`HubIdentityExchange`] for offline tests and local demos.
///
/// Non-destructive, unlike a single-use code: a platform token is a bearer
/// credential with a lifetime, so presenting it twice legitimately succeeds
/// twice. A mock that expired it on first use would make the route look
/// stricter than it is.
#[derive(Debug, Default)]
pub struct MockHubIdentityExchange {
    tokens: StdMutex<HashMap<String, String>>,
    /// Grant codes and the `(verifier, key)` each redeems to.
    ///
    /// Single-use, unlike [`Self::tokens`]: a grant code really is spent on
    /// redemption at the hub, and a mock that let one be redeemed twice would
    /// make the route look safe to retry when it is not.
    grants: StdMutex<HashMap<String, (String, String)>>,
    /// A forced transport failure, standing in for "the hub is not answering".
    unreachable: bool,
    /// What [`HubIdentityExchange::billing_summary`] answers, per key.
    billing: StdMutex<HashMap<String, BillingSummary>>,
}

impl MockHubIdentityExchange {
    /// An exchange that knows no tokens; every lookup is rejected.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds one live token and the address it resolves to.
    pub fn with_token(self, token: &str, email: &str) -> Self {
        self.tokens
            .lock()
            .expect("mock poisoned")
            .insert(token.to_string(), email.to_string());
        self
    }

    /// Seeds the billing standing one key reads back.
    pub fn with_billing(self, key: &str, summary: BillingSummary) -> Self {
        self.billing
            .lock()
            .expect("mock poisoned")
            .insert(key.to_string(), summary);
        self
    }

    /// Seeds one grant code, the verifier that unlocks it, and the key it mints.
    pub fn with_grant(self, code: &str, verifier: &str, key: &str) -> Self {
        self.grants
            .lock()
            .expect("mock poisoned")
            .insert(code.to_string(), (verifier.to_string(), key.to_string()));
        self
    }

    /// An exchange whose hub cannot be reached at all.
    ///
    /// Distinct from an unknown token on purpose: one is a dead credential the
    /// caller should re-earn by signing in again, the other is an outage the
    /// caller can do nothing about, and the route must not tell someone to
    /// click again when clicking again cannot work.
    pub fn unreachable() -> Self {
        Self {
            unreachable: true,
            ..Self::default()
        }
    }
}

/// The error the hub returns for a token that is forged, expired, or revoked.
///
/// One shape for all three, mirroring the hub: `GET /auth/me` answers 401 for
/// every one of them, and inventing a finer distinction here would be this
/// tenant guessing at a fact only the hub holds.
fn rejected() -> crate::error::OpenCompanyError {
    crate::error::OpenCompanyError::TinyHumans {
        code: "http_401".to_string(),
        message: "The hub did not recognize that sign-in".to_string(),
    }
}

#[async_trait]
impl HubIdentityExchange for MockHubIdentityExchange {
    async fn identify(&self, token: &str) -> Result<HubIdentity> {
        if self.unreachable {
            return Err(crate::error::OpenCompanyError::TinyHumans {
                code: "unreachable".to_string(),
                message: "connection refused".to_string(),
            });
        }
        self.tokens
            .lock()
            .expect("mock poisoned")
            .get(token)
            .map(|email| HubIdentity {
                email: email.clone(),
            })
            .ok_or_else(rejected)
    }

    async fn redeem_key_grant(&self, code: &str, verifier: &str) -> Result<String> {
        if self.unreachable {
            return Err(crate::error::OpenCompanyError::TinyHumans {
                code: "unreachable".to_string(),
                message: "connection refused".to_string(),
            });
        }
        // Removed before the verifier is checked, mirroring the hub: a wrong
        // verifier spends the code rather than leaving it up for another guess.
        let entry = self.grants.lock().expect("mock poisoned").remove(code);
        match entry {
            Some((expected, key)) if expected == verifier => Ok(key),
            _ => Err(rejected()),
        }
    }

    async fn billing_summary(&self, key: &str) -> Result<BillingSummary> {
        if self.unreachable {
            return Err(crate::error::OpenCompanyError::TinyHumans {
                code: "unreachable".to_string(),
                message: "connection refused".to_string(),
            });
        }
        // Non-destructive, like `identify` and unlike a grant code: reading a
        // balance twice is the same read twice.
        self.billing
            .lock()
            .expect("mock poisoned")
            .get(key)
            .cloned()
            .ok_or_else(rejected)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// The exact string the hub's gate receives from a hosted console.
    ///
    /// Pinned because it is the thing `tinyhumansai/backend#1243` has to accept,
    /// and it is **not** a bare origin — the `?company=` rides along. A gate that
    /// compares this string against a registry of provisioned origins rejects
    /// every real request and reproduces issue #512 exactly; only the origin
    /// component is stable, and in shared-single-DB mode the company id is
    /// namespaced `<tenant>--<id>` and varies per tenant and over time.
    #[test]
    fn a_hosted_start_url_carries_the_tenant_origin_and_its_company() {
        let start = login_start_url(
            "https://hub.example.com",
            "google",
            "https://smoke1.example.com/?company=smoke1",
        );

        assert_eq!(
            start,
            "https://hub.example.com/auth/google/login\
             ?redirectUri=https%3A%2F%2Fsmoke1.example.com%2F%3Fcompany%3Dsmoke1"
        );
    }

    /// The exact string the hub's `GET /auth/key` receives, pinned whole.
    ///
    /// Every value percent-encoded the same way, `scopes` included: the hub
    /// reads it off the query string like any other parameter, and an encoder
    /// applied to three of four values is the one that surprises somebody.
    #[test]
    fn a_key_grant_query_asks_for_its_scopes_beside_the_challenge() {
        assert_eq!(
            key_grant_query("https://acme.example.com/?company=acme", "chal-abc", "Acme"),
            "callback_url=https%3A%2F%2Facme.example.com%2F%3Fcompany%3Dacme\
             &code_challenge=chal-abc\
             &code_challenge_method=S256\
             &name=Acme\
             &scopes=connections"
        );
    }

    /// Without this name on the ask, a key minted to anything but a provisioned
    /// tenant cannot drive `/agent-integrations/composio/*` at all.
    #[test]
    fn the_scopes_asked_for_name_connections() {
        assert!(
            KEY_GRANT_SCOPES.contains(&"connections"),
            "managed Composio 403s without it: {KEY_GRANT_SCOPES:?}"
        );
    }

    /// Both readers get the builder's output verbatim.
    ///
    /// The property the split exists for: neither path may append a parameter
    /// of its own, because a grant whose reach depends on which page the
    /// browser went through is one nobody can reason about from the consent
    /// screen.
    #[test]
    fn both_readers_carry_the_same_built_query() {
        let query = key_grant_query("http://127.0.0.1:5173/?key=link", "chal-abc", "Acme");

        let api = key_grant_url(
            "https://api.example.com",
            "http://127.0.0.1:5173/?key=link",
            "chal-abc",
            "Acme",
        );
        let site = crate::server::hub_account::connect_url("https://example.com", &query);

        assert_eq!(api, format!("https://api.example.com/auth/key?{query}"));
        assert_eq!(site, format!("https://example.com/connect?{query}"));
    }
}

/// The real HTTP exchange, compiled only under the `tinyhumans` feature.
#[cfg(feature = "tinyhumans")]
pub use http::HttpHubIdentityExchange;

#[cfg(feature = "tinyhumans")]
mod http {
    use super::{BillingSummary, HubIdentity, HubIdentityExchange};
    use crate::Result;
    use crate::error::OpenCompanyError;
    use async_trait::async_trait;
    use serde::Deserialize;

    /// The hub's envelope for `GET /auth/me`.
    #[derive(Debug, Deserialize)]
    struct MeResponse {
        data: MeData,
    }

    #[derive(Debug, Deserialize)]
    struct MeData {
        email: String,
    }

    /// The hub's envelope for `POST /auth/keys`.
    #[derive(Debug, Deserialize)]
    struct KeyResponse {
        data: KeyData,
    }

    #[derive(Debug, Deserialize)]
    struct KeyData {
        /// The plaintext key. The hub emits it exactly once.
        key: String,
    }

    /// The hub's envelope for `GET /payments/summary`.
    #[derive(Debug, Deserialize)]
    struct SummaryResponse {
        data: SummaryData,
    }

    #[derive(Debug, Deserialize)]
    struct SummaryData {
        #[serde(default)]
        credits: SummaryCredits,
        #[serde(default)]
        plan: SummaryPlan,
        #[serde(default)]
        links: SummaryLinks,
    }

    #[derive(Debug, Default, Deserialize)]
    struct SummaryCredits {
        #[serde(rename = "totalUsd", default)]
        total_usd: f64,
    }

    #[derive(Debug, Default, Deserialize)]
    struct SummaryPlan {
        #[serde(default)]
        plan: Option<String>,
        #[serde(rename = "hasActiveSubscription", default)]
        has_active_subscription: bool,
        #[serde(rename = "planExpiry", default)]
        plan_expiry: Option<String>,
    }

    #[derive(Debug, Default, Deserialize)]
    struct SummaryLinks {
        #[serde(rename = "topUpUrl", default)]
        top_up_url: Option<String>,
        #[serde(rename = "manageUrl", default)]
        manage_url: Option<String>,
    }

    /// A [`HubIdentityExchange`] backed by `GET {api_url}/auth/me`.
    ///
    /// Deliberately the hub's *existing* session route rather than anything
    /// built for tenants. There is no new endpoint to secure, no new token type
    /// to expire, and no way for this call to learn more than the person who
    /// presented the token already knows about themselves.
    pub struct HttpHubIdentityExchange {
        api_url: String,
        http: reqwest::Client,
    }

    impl HttpHubIdentityExchange {
        /// Builds an exchange against `api_url`.
        pub fn new(api_url: impl Into<String>) -> Self {
            Self {
                // Trailing slashes would produce `//auth/me`.
                api_url: api_url.into().trim_end_matches('/').to_string(),
                http: reqwest::Client::new(),
            }
        }

        fn err(context: &str, e: impl std::fmt::Display) -> OpenCompanyError {
            OpenCompanyError::TinyHumans {
                code: context.to_string(),
                message: e.to_string(),
            }
        }
    }

    #[async_trait]
    impl HubIdentityExchange for HttpHubIdentityExchange {
        async fn identify(&self, token: &str) -> Result<HubIdentity> {
            let url = format!("{}/auth/me", self.api_url);
            let (product_header_name, product_header_value) =
                crate::product::product_identity_header();
            let resp = self
                .http
                .get(&url)
                .bearer_auth(token)
                // `api_url` is the TinyHumans backend itself (`AppConfig::api_url`,
                // defaulting to `crate::app::config::DEFAULT_API_URL`), so this is
                // our own backend and is tagged like every other call we make to
                // it. A bespoke `reqwest::Client`, not one built through
                // `openhuman_core`'s `IntegrationClient`, so it never inherits the
                // header `set_product_identity` attaches — see `crate::product`.
                .header(product_header_name, product_header_value)
                .send()
                .await
                .map_err(|e| Self::err("unreachable", e))?;

            let status = resp.status();
            if !status.is_success() {
                // The hub's own message is safe to surface — it describes the
                // token's standing, never the person's. The token itself is
                // never echoed, and `reqwest`'s error Display would not carry
                // it either (the bearer lives in a header, not the URL).
                let detail = resp.text().await.unwrap_or_default();
                return Err(Self::err(
                    &format!("http_{}", status.as_u16()),
                    truncate(&detail, 200),
                ));
            }

            let parsed: MeResponse = resp.json().await.map_err(|e| Self::err("decode", e))?;
            Ok(HubIdentity {
                email: parsed.data.email,
            })
        }

        async fn redeem_key_grant(&self, code: &str, verifier: &str) -> Result<String> {
            let url = format!("{}/auth/keys", self.api_url);
            let (product_header_name, product_header_value) =
                crate::product::product_identity_header();
            // No bearer: the hub's redemption route is unauthenticated, and the
            // verifier is what authenticates it. That is the whole point of the
            // exchange — this host never holds a credential belonging to the
            // person who approved the grant.
            let resp = self
                .http
                .post(&url)
                .header(product_header_name, product_header_value)
                .json(&serde_json::json!({ "code": code, "code_verifier": verifier }))
                .send()
                .await
                .map_err(|e| Self::err("unreachable", e))?;

            let status = resp.status();
            if !status.is_success() {
                // The hub's message describes the code's standing ("invalid or
                // expired", "verifier does not match"), never the key. Neither
                // argument is echoed: both are live secrets, and the response
                // body is the hub's own words about its own flow.
                let detail = resp.text().await.unwrap_or_default();
                return Err(Self::err(
                    &format!("http_{}", status.as_u16()),
                    truncate(&detail, 200),
                ));
            }

            let parsed: KeyResponse = resp.json().await.map_err(|e| Self::err("decode", e))?;
            Ok(parsed.data.key)
        }

        async fn billing_summary(&self, key: &str) -> Result<BillingSummary> {
            let url = format!("{}/payments/summary", self.api_url);
            let (product_header_name, product_header_value) =
                crate::product::product_identity_header();
            let resp = self
                .http
                .get(&url)
                .bearer_auth(key)
                .header(product_header_name, product_header_value)
                .send()
                .await
                .map_err(|e| Self::err("unreachable", e))?;

            let status = resp.status();
            if !status.is_success() {
                // The hub's words describe the key's standing — expired, revoked,
                // wrong scope. The key is in a header, so neither the body nor
                // `reqwest`'s Display can carry it into this error.
                let detail = resp.text().await.unwrap_or_default();
                return Err(Self::err(
                    &format!("http_{}", status.as_u16()),
                    truncate(&detail, 200),
                ));
            }

            let parsed: SummaryResponse = resp.json().await.map_err(|e| Self::err("decode", e))?;
            Ok(BillingSummary {
                balance_usd: parsed.data.credits.total_usd,
                // A hub that names no plan is on the free one — the field is
                // absent there rather than spelled out, and a card reading
                // "unknown" would be a worse answer than the true one.
                plan: parsed.data.plan.plan.unwrap_or_else(|| "free".to_string()),
                active_subscription: parsed.data.plan.has_active_subscription,
                plan_expiry: parsed.data.plan.plan_expiry,
                top_up_url: parsed.data.links.top_up_url,
                manage_url: parsed.data.links.manage_url,
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
