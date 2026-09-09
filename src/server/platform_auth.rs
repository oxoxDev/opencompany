//! Platform authentication: the hosting layer's tenant-scoped bearer.
//!
//! This is the only *machine* credential. A platform-issued bearer's verified
//! [`PlatformClaims`] carry a `tenant`, a set of `scopes`, and an optional
//! company allow-list. Provisioning and suspension require the `platform`
//! scope; every tenant token is confined to the companies it owns, so it can
//! never cross tenants.
//!
//! Humans do not use this surface — they sign in and carry a session cookie
//! (see [`server::users`](crate::server::users)). Without `platform_auth`
//! configured there is no machine credential at all, and a session is the only
//! way in.
//!
//! The verification seam is [`PlatformVerifier`]. A shipped build authenticates
//! a bearer exactly two ways, and [`configure`] selects between them from the
//! environment:
//!
//! - [`StaticPlatformVerifier`] — an exact match against a shared platform
//!   secret. Authenticated by *knowledge of the secret*, the same pattern the
//!   control plane uses for its own admin token.
//! - [`JwtPlatformVerifier`] — HS256-signed, tenant-scoped claims. This is how a
//!   tenant token is issued in production.
//!
//! A host may set either or both; with both, a bearer is accepted if either
//! accepts it. Neither set means there is no machine credential at all.
//!
//! No shipped build parses caller-supplied claims that carry no authentication.
//! The offline `oc_tenant.<base64url(json)>` codec survives only as
//! `UnsignedTenantVerifier`, a `cfg(test)` type, so the scope-gate,
//! allow-list and ownership suites keep running without signing machinery. It is
//! a separate type rather than a branch inside `StaticPlatformVerifier::verify`
//! on purpose: a `cfg(test)` branch there would make the production rejection
//! unobservable, because the test build would still accept the unsigned shape.
//!
//! Known limits, stated rather than fixed here:
//!
//! - The signing is **symmetric** — a workload that can verify can also mint.
//!   Acceptable while the trust boundary is a single workload; asymmetric keys
//!   via the control plane's issuer are the follow-up.
//! - A signed token carrying **no `exp` never expires**, because the verifier
//!   clears its required-claims set. Changing that is a separate policy call.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::{FromRequestParts, RawPathParams};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::{Json, http::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::error::OpenCompanyError;
use crate::ports::types::CompanyId;
use crate::server::graphql::auth::GqlAuth;

/// The `platform` scope, required for provisioning and suspension.
pub const SCOPE_PLATFORM: &str = "platform";

/// Claims a verified platform token carries.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlatformClaims {
    /// The owning tenant, e.g. `tenant:acme`.
    pub tenant: String,
    /// The granted scopes, e.g. `{"operator", "platform"}`.
    #[serde(default)]
    pub scopes: HashSet<String>,
    /// An explicit company allow-list. `None` means "any company this tenant
    /// owns" (ownership is enforced separately against the registry map).
    #[serde(default)]
    pub companies: Option<HashSet<String>>,
}

impl PlatformClaims {
    /// Whether these claims carry the `platform` scope (provisioning/suspension).
    pub fn has_platform_scope(&self) -> bool {
        self.scopes.contains(SCOPE_PLATFORM)
    }

    /// Whether the token's own allow-list permits addressing `id`. `None`
    /// allow-list permits any id (ownership is checked separately).
    pub fn may_address(&self, id: &CompanyId) -> bool {
        match &self.companies {
            Some(allow) => allow.contains(id.as_ref()),
            None => true,
        }
    }
}

/// The verification seam: turns a bearer string into [`PlatformClaims`] or an
/// error. Implementations are offline-testable; only real JWT signature
/// verification is feature-gated.
pub trait PlatformVerifier: Send + Sync {
    /// Verifies `bearer` and returns its claims, or an error if invalid.
    fn verify(&self, bearer: &str) -> crate::Result<PlatformClaims>;
}

/// The shared-secret verifier: a bearer *equal to* `platform_secret` is a
/// full platform-scope token, and nothing else is accepted.
///
/// Exact match is the whole mechanism, and it is a real one — the credential is
/// authenticated by knowledge of the secret, so a caller who does not hold it
/// cannot produce an accepted bearer. Tenant-scoped machine tokens are signed
/// JWTs ([`JwtPlatformVerifier`]); this verifier issues only the one
/// `tenant:platform` identity.
#[derive(Clone)]
pub struct StaticPlatformVerifier {
    /// The shared secret that grants a full platform-scope token.
    pub platform_secret: String,
}

impl StaticPlatformVerifier {
    /// Builds a verifier around a shared platform secret.
    pub fn new(platform_secret: impl Into<String>) -> Self {
        Self {
            platform_secret: platform_secret.into(),
        }
    }
}

impl PlatformVerifier for StaticPlatformVerifier {
    fn verify(&self, bearer: &str) -> crate::Result<PlatformClaims> {
        if constant_time_eq(bearer, &self.platform_secret) {
            return Ok(PlatformClaims {
                tenant: "tenant:platform".to_string(),
                scopes: HashSet::from([SCOPE_PLATFORM.to_string(), "operator".to_string()]),
                companies: None,
            });
        }
        Err(OpenCompanyError::InvalidRequest(
            "unrecognized token".to_string(),
        ))
    }
}

/// Compares two strings without the short-circuit a plain `==` on `&str`
/// takes at the first differing byte.
///
/// [`StaticPlatformVerifier::verify`] is the whole authentication mechanism
/// for the shared platform secret: knowledge of the exact value is what grants
/// a full platform-scope token. A short-circuiting byte compare turns "how
/// long did verification take" into a per-byte oracle over that secret, which
/// is exactly the shape a timing attack walks a guess forward one correct byte
/// at a time. This still runs in time proportional to the **longer** input
/// (so a caller can still learn there was a length mismatch from timing alone,
/// same as `subtle::ConstantTimeEq` and every other implementation of this
/// pattern) — what it removes is the byte-position leak `==` has once lengths
/// already match, which is the exploitable half against a fixed-length secret.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// The signed-JWT verifier (HS256) for tenant-scoped machine tokens. The claim
/// shape mirrors [`PlatformClaims`] (`tenant`, `scopes`, `companies`); `exp` is
/// honored when present.
///
/// Gated on `platform-jwt`, which is in the default feature set — a build
/// without it can verify nothing but the shared secret, and [`configure`]
/// refuses to boot rather than silently ignoring a signing secret it cannot use.
#[cfg(feature = "platform-jwt")]
pub struct JwtPlatformVerifier {
    secret: String,
}

#[cfg(feature = "platform-jwt")]
impl JwtPlatformVerifier {
    /// Builds an HS256 verifier around a shared signing secret.
    pub fn new(secret: impl Into<String>) -> Self {
        Self {
            secret: secret.into(),
        }
    }
}

#[cfg(feature = "platform-jwt")]
impl PlatformVerifier for JwtPlatformVerifier {
    fn verify(&self, bearer: &str) -> crate::Result<PlatformClaims> {
        use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};

        let mut validation = Validation::new(Algorithm::HS256);
        // Callers may omit registered claims; only signature (and `exp` when
        // present) matter for the platform gate.
        validation.required_spec_claims.clear();
        validation.validate_exp = true;

        let token = decode::<PlatformClaims>(
            bearer,
            &DecodingKey::from_secret(self.secret.as_bytes()),
            &validation,
        )
        .map_err(|e| OpenCompanyError::InvalidRequest(format!("invalid jwt: {e}")))?;
        Ok(token.claims)
    }
}

/// Platform auth configuration held on [`AppConfig`](crate::AppConfig): the
/// verifier plus optional expected issuer/audience (used by the JWT verifier).
#[derive(Clone)]
pub struct PlatformAuthConfig {
    /// The token verifier.
    pub verifier: Arc<dyn PlatformVerifier>,
}

impl PlatformAuthConfig {
    /// Builds a config around a verifier.
    pub fn new(verifier: Arc<dyn PlatformVerifier>) -> Self {
        Self { verifier }
    }
}

impl std::fmt::Debug for PlatformAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformAuthConfig").finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Boot-time selection
// ---------------------------------------------------------------------------

/// Environment variable holding the shared platform secret. Unset means the
/// shared-secret credential does not exist; there is no shipped default.
pub const PLATFORM_TOKEN_ENV: &str = "OPENCOMPANY_PLATFORM_TOKEN";

/// Environment variable holding the HS256 secret that signs tenant tokens.
/// Unset means signed tenant tokens are not accepted; there is no shipped
/// default and no fallback — an absent value is an absent capability, never a
/// weaker one.
pub const PLATFORM_JWT_SECRET_ENV: &str = "OPENCOMPANY_PLATFORM_JWT_SECRET";

/// Which credential shapes a running host accepts. Printed at boot so an
/// operator can read the active mode off the logs; carries no secret material
/// and is not derived from any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformAuthMode {
    /// Only the exact-match shared platform secret.
    SharedSecret,
    /// Only signed tenant tokens.
    Jwt,
    /// Both; a bearer accepted by either is accepted.
    Both,
}

impl PlatformAuthMode {
    /// The mode's log name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SharedSecret => "shared-secret",
            Self::Jwt => "jwt",
            Self::Both => "both",
        }
    }
}

impl std::fmt::Display for PlatformAuthMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Accepts a bearer that any constituent verifier accepts, in order.
///
/// Only used when a host configures more than one credential shape. The last
/// error is surfaced when every verifier refuses — callers turn any error into
/// a flat `401`, so which one it is never reaches the wire.
struct AnyPlatformVerifier {
    verifiers: Vec<Arc<dyn PlatformVerifier>>,
}

impl PlatformVerifier for AnyPlatformVerifier {
    fn verify(&self, bearer: &str) -> crate::Result<PlatformClaims> {
        let mut last = None;
        for verifier in &self.verifiers {
            match verifier.verify(bearer) {
                Ok(claims) => return Ok(claims),
                Err(err) => last = Some(err),
            }
        }
        Err(last
            .unwrap_or_else(|| OpenCompanyError::InvalidRequest("unrecognized token".to_string())))
    }
}

/// Whether this build can verify a signed tenant token at all.
const JWT_AVAILABLE: bool = cfg!(feature = "platform-jwt");

/// The boot refusal for a signing secret this build cannot use. Names the
/// variable and the missing feature; never the value.
fn jwt_unavailable() -> OpenCompanyError {
    OpenCompanyError::Config(format!(
        "{PLATFORM_JWT_SECRET_ENV} is set but this build has no `platform-jwt` feature, \
         so a signed tenant token cannot be verified. Rebuild with \
         `--features platform-jwt` (it is in the default set) or unset the variable."
    ))
}

#[cfg(feature = "platform-jwt")]
fn jwt_verifier(secret: String) -> crate::Result<Arc<dyn PlatformVerifier>> {
    Ok(Arc::new(JwtPlatformVerifier::new(secret)))
}

#[cfg(not(feature = "platform-jwt"))]
fn jwt_verifier(_secret: String) -> crate::Result<Arc<dyn PlatformVerifier>> {
    Err(jwt_unavailable())
}

/// Builds the platform auth config from the two credential secrets, or `None`
/// when neither is set (no machine credential — humans and `serve --company`
/// are the whole story).
///
/// Every configuration state either authenticates or refuses; none of them
/// fails open. No credential is `None` and every machine route 401s; a signing
/// secret on a build that cannot verify signatures aborts boot with
/// `jwt_unavailable` rather than degrading to the shared secret. A blank or
/// whitespace-only value is treated as unset, so an empty injected variable
/// cannot become an empty accepted bearer.
pub fn configure(
    platform_secret: Option<String>,
    jwt_secret: Option<String>,
) -> crate::Result<Option<(PlatformAuthConfig, PlatformAuthMode)>> {
    configure_with(platform_secret, jwt_secret, JWT_AVAILABLE)
}

/// [`configure`] with the build's JWT support passed in, so the refusal path is
/// reachable from a test on a build that *does* have the feature.
fn configure_with(
    platform_secret: Option<String>,
    jwt_secret: Option<String>,
    jwt_available: bool,
) -> crate::Result<Option<(PlatformAuthConfig, PlatformAuthMode)>> {
    let platform_secret = platform_secret.filter(|value| !value.trim().is_empty());
    let jwt_secret = jwt_secret.filter(|value| !value.trim().is_empty());

    let mode = match (platform_secret.is_some(), jwt_secret.is_some()) {
        (false, false) => return Ok(None),
        (true, false) => PlatformAuthMode::SharedSecret,
        (false, true) => PlatformAuthMode::Jwt,
        (true, true) => PlatformAuthMode::Both,
    };

    let mut verifiers: Vec<Arc<dyn PlatformVerifier>> = Vec::new();
    if let Some(secret) = platform_secret {
        verifiers.push(Arc::new(StaticPlatformVerifier::new(secret)));
    }
    if let Some(secret) = jwt_secret {
        if !jwt_available {
            return Err(jwt_unavailable());
        }
        verifiers.push(jwt_verifier(secret)?);
    }

    let verifier: Arc<dyn PlatformVerifier> = if verifiers.len() == 1 {
        verifiers.pop().expect("one verifier")
    } else {
        Arc::new(AnyPlatformVerifier { verifiers })
    };
    Ok(Some((PlatformAuthConfig::new(verifier), mode)))
}

/// Extracts the bearer token from the `Authorization` header.
///
/// Shared with [`resolve_claims`](crate::server::graphql::auth::resolve_claims)
/// so REST and GraphQL parse the credential identically.
pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

/// Refuses a user who is still carrying an admin-issued temporary password.
///
/// An admin who resets a password knows it, and conveys it over some channel
/// they do not control. So a session opened with one is only good for replacing
/// it: this returns `403 password_change_required` everywhere except the auth
/// routes (set-password, logout, me), which deliberately do not call this so
/// the user can always resolve the situation.
///
/// Checked at the extractors rather than surfaced to the console, so it holds
/// against a client that would rather not honor it.
pub(crate) fn refuse_until_password_changed(auth: &GqlAuth) -> Option<Response> {
    match auth {
        GqlAuth::User(user) if user.must_change_password => Some(
            (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "set a new password before continuing",
                    "code": "password_change_required",
                })),
            )
                .into_response(),
        ),
        _ => None,
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized", "code": "unauthorized" })),
    )
        .into_response()
}

pub(crate) fn forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": "forbidden", "code": "forbidden" })),
    )
        .into_response()
}

/// An extractor for any principal entitled to address a company: a platform
/// token, or a human's session cookie.
///
/// Replaces the old `PlatformOrOperatorAuth`, whose `Option<PlatformClaims>`
/// could not represent a human and whose `None` meant "dev mode, allow
/// everything". There is no such state now — an unauthenticated request is
/// `401`.
///
/// The extractor resolves the addressed company from the `{id}` path param when
/// present so a session cookie can be matched to it; on the single-company
/// alias the registry's sole company is the addressed one.
///
/// ## Addressing is enforced here, not left to the handler
///
/// When the path names a company this host serves, this runs
/// [`authorize_address`] itself. Every handler already made that call — and had
/// to, or it was open to any verified principal on the host — but nothing in
/// the type system said so: the extractor handed back a principal that had only
/// *authenticated*, and a route that forgot the follow-up was cross-company-open
/// with no compile error and no failing test. Answering it in the extractor
/// makes the safe shape the default one. Handlers that still call it are
/// unaffected: the second call is the same decision over the same inputs.
///
/// Deliberately scoped to a company the registry actually holds. An id this
/// host does not serve is a *not found*, and every route that resolves one says
/// so; turning it into a `403` here would reorder those answers and disclose
/// nothing useful in exchange.
pub struct CompanyAuth(pub GqlAuth);

impl FromRequestParts<AppState> for CompanyAuth {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        use crate::server::graphql::auth::resolve_principal;

        // Sniff `{id}` without consuming it; handlers still extract their own.
        let company = RawPathParams::from_request_parts(parts, state)
            .await
            .ok()
            .and_then(|params| {
                params
                    .iter()
                    .find(|(key, _)| *key == "id")
                    .map(|(_, value)| CompanyId::new(value))
            });
        let peer = parts
            .extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|info| info.0);
        let auth = resolve_principal(&parts.headers, state, company.as_ref(), peer)
            .await
            .map_err(|_| unauthorized())?;
        if let Some(id) = company.filter(|id| state.registry().get(id).is_some())
            && let Some(resp) = authorize_address(state, &auth, &id)
        {
            return Err(resp);
        }
        Ok(Self(auth))
    }
}

/// An extractor requiring the `platform` scope: the hosting layer only.
///
/// This gates provisioning and suspension — creating and destroying companies
/// across tenants. It resolves through [`resolve_claims`], which cannot return
/// a human, so a session cookie can never reach these routes whatever it
/// contains.
///
/// Without `platform_auth` configured nobody holds the scope, so a self-hosted
/// deployment has no HTTP provisioning at all and loads companies with
/// `serve --company <dir>`. That is the intended shape: a prosumer host has no
/// machine credential to hand out.
pub struct PlatformScope(pub PlatformClaims);

impl FromRequestParts<AppState> for PlatformScope {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        use crate::server::graphql::auth::{GqlAuth, resolve_claims};
        match resolve_claims(&parts.headers, state) {
            Ok(GqlAuth::Platform(claims)) if claims.has_platform_scope() => Ok(Self(claims)),
            Ok(GqlAuth::Platform(_)) => Err(forbidden()),
            // Unreachable: resolve_claims cannot construct a User. Stated
            // rather than wildcarded so that if it ever could, this refuses.
            Ok(GqlAuth::User(_)) => Err(forbidden()),
            Err(_) => Err(unauthorized()),
        }
    }
}

/// Authorizes addressing a specific company under the given claims.
///
/// - Platform scope may address any company.
/// - A tenant token may address a company only when it owns it (the registry
///   ownership map records `id -> tenant`) and its own allow-list permits it.
/// - `None` claims (prosumer/dev) are always allowed.
///
/// Returns `Some(403 forbidden)` on a cross-tenant or out-of-allow-list attempt,
/// or `None` when the caller is allowed to address `id`.
pub fn authorize_address(state: &AppState, auth: &GqlAuth, id: &CompanyId) -> Option<Response> {
    match auth {
        GqlAuth::Platform(claims) => {
            if claims.has_platform_scope() {
                return None;
            }
            let owner = state.owner_of(id);
            if owner.as_deref() == Some(crate::app::canonical_tenant(&claims.tenant))
                && claims.may_address(id)
            {
                None
            } else {
                Some(forbidden())
            }
        }
        // A user belongs to one company. The storage partition already makes a
        // cross-company session unresolvable; this is the explicit check.
        GqlAuth::User(user) => {
            if user.company == *id {
                None
            } else {
                Some(forbidden())
            }
        }
    }
}

/// The tenant a token acts as, for ownership recording. Platform-scope and dev
/// callers act as the `tenant:platform` account.
pub fn acting_tenant(auth: &GqlAuth) -> String {
    match auth {
        GqlAuth::Platform(claims) => claims.tenant.clone(),
        // A human acts for the company they belong to. Ownership records a
        // tenant, and a self-hosted company's tenant is itself.
        GqlAuth::User(user) => format!("company:{}", user.company),
    }
}

// ---------------------------------------------------------------------------
// base64url (no padding) — std-only, used by the offline dev token codec.
// ---------------------------------------------------------------------------

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Encodes bytes as unpadded base64url.
pub(crate) fn b64url_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL[(n >> 18) as usize & 0x3f] as char);
        out.push(B64URL[(n >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(B64URL[(n >> 6) as usize & 0x3f] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL[n as usize & 0x3f] as char);
        }
    }
    out
}

/// Decodes unpadded base64url, returning `None` on any invalid input.
///
/// `cfg(test)` — the only decoder in this module belonged to the unsigned token
/// arm, which no longer exists in a shipped build. It stays for
/// `UnsignedTenantVerifier` and the round-trip test.
#[cfg(test)]
fn b64url_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            return None;
        }
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// A test-only verifier that additionally accepts an **unsigned**
/// `oc_tenant.<base64url(json)>` bearer as literal claims.
///
/// This is how the scope-gate, allow-list and ownership suites mint a
/// tenant-scoped principal without standing up signing machinery: those suites
/// exercise what a *verified* tenant token may reach, which is orthogonal to how
/// it was authenticated. It is `cfg(test)`, so no shipped build contains it, and
/// it is a distinct type from [`StaticPlatformVerifier`] precisely so that the
/// production refusal of this shape stays observable to
/// `hand_constructed_tenant_bearer_is_refused`.
#[cfg(test)]
pub struct UnsignedTenantVerifier {
    inner: StaticPlatformVerifier,
}

#[cfg(test)]
impl UnsignedTenantVerifier {
    /// The prefix marking an unsigned structured tenant token.
    pub const TENANT_PREFIX: &'static str = "oc_tenant.";

    /// Builds a verifier that also honors the shared platform secret, so a
    /// suite can hold both principals at once.
    pub fn new(platform_secret: impl Into<String>) -> Self {
        Self {
            inner: StaticPlatformVerifier::new(platform_secret),
        }
    }

    /// Encodes claims into an unsigned tenant token.
    pub fn tenant_token(claims: &PlatformClaims) -> String {
        let json = serde_json::to_vec(claims).expect("claims serialize");
        format!("{}{}", Self::TENANT_PREFIX, b64url_encode(&json))
    }
}

#[cfg(test)]
impl PlatformVerifier for UnsignedTenantVerifier {
    fn verify(&self, bearer: &str) -> crate::Result<PlatformClaims> {
        match bearer.strip_prefix(Self::TENANT_PREFIX) {
            Some(encoded) => {
                let bytes = b64url_decode(encoded).ok_or_else(|| {
                    OpenCompanyError::InvalidRequest("malformed platform token".to_string())
                })?;
                Ok(serde_json::from_slice(&bytes)?)
            }
            // Delegate so the suites keep exercising the real secret arm.
            None => self.inner.verify(bearer),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn tenant_claims(tenant: &str, scopes: &[&str]) -> PlatformClaims {
        PlatformClaims {
            tenant: tenant.to_string(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            companies: None,
        }
    }

    #[test]
    fn b64url_round_trips() {
        for sample in [&b""[..], b"a", b"ab", b"abc", b"abcd", b"hello world!"] {
            let encoded = b64url_encode(sample);
            assert_eq!(b64url_decode(&encoded).unwrap(), sample);
        }
    }

    #[test]
    fn platform_secret_grants_platform_scope() {
        let verifier = StaticPlatformVerifier::new("top-secret");
        let claims = verifier.verify("top-secret").unwrap();
        assert!(claims.has_platform_scope());
        assert_eq!(claims.tenant, "tenant:platform");
    }

    /// The offline codec the gate suites mint tenant principals with. It lives
    /// on the test-only type; the assertion below is about the codec, not about
    /// anything a shipped build accepts.
    #[test]
    fn unsigned_codec_round_trips_for_the_gate_suites() {
        let verifier = UnsignedTenantVerifier::new("top-secret");
        let token =
            UnsignedTenantVerifier::tenant_token(&tenant_claims("tenant:acme", &["operator"]));
        let claims = verifier.verify(&token).unwrap();
        assert_eq!(claims.tenant, "tenant:acme");
        assert!(!claims.has_platform_scope());

        // It still delegates the shared-secret arm, so a suite can hold both.
        assert!(verifier.verify("top-secret").unwrap().has_platform_scope());
    }

    /// The shipped default build must not turn a caller-supplied payload into
    /// platform claims. The bearer is assembled literally, from nothing but the
    /// wire shape, so this stays a black-box statement about what an
    /// unauthenticated request can obtain — no helper, no shared constant.
    #[test]
    fn hand_constructed_tenant_bearer_is_refused() {
        let verifier = StaticPlatformVerifier::new("top-secret");
        let forged = format!(
            "oc_tenant.{}",
            b64url_encode(br#"{"tenant":"tenant:victim","scopes":["platform","operator"]}"#)
        );
        assert!(
            verifier.verify(&forged).is_err(),
            "an unsigned, caller-supplied payload must not resolve to platform claims"
        );
    }

    #[cfg(feature = "platform-jwt")]
    fn sign(secret: &str, claims: &serde_json::Value) -> String {
        use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
        encode(
            &Header::new(Algorithm::HS256),
            claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .expect("sign")
    }

    #[cfg(feature = "platform-jwt")]
    #[test]
    fn jwt_verifier_refuses_an_expired_token() {
        let secret = "signing-secret";
        // 2001-09-09, comfortably in the past whenever this runs.
        let token = sign(
            secret,
            &json!({"tenant": "tenant:acme", "scopes": ["operator"], "exp": 1_000_000_000u64}),
        );
        assert!(JwtPlatformVerifier::new(secret).verify(&token).is_err());
    }

    /// Pins down a documented, deliberately-deferred limit (this module's own
    /// doc comment: "A signed token carrying no `exp` never expires... \
    /// Changing that is a separate policy call") rather than changing it. A
    /// token with no `exp` claim at all is accepted, and stays accepted
    /// however long from now this runs — there is no lever in this verifier
    /// that would ever refuse it on staleness alone. If a future change adds
    /// a mandatory-expiry policy, this test is the one that should start
    /// failing and prompt updating it, rather than the behavior silently
    /// drifting either direction unnoticed.
    #[cfg(feature = "platform-jwt")]
    #[test]
    fn jwt_verifier_accepts_a_token_with_no_exp_claim_at_all() {
        let secret = "signing-secret";
        let token = sign(
            secret,
            &json!({"tenant": "tenant:acme", "scopes": ["operator"]}),
        );
        let claims = JwtPlatformVerifier::new(secret)
            .verify(&token)
            .expect("a token with no exp claim is currently accepted unconditionally");
        assert_eq!(claims.tenant, "tenant:acme");
    }

    #[cfg(feature = "platform-jwt")]
    #[test]
    fn jwt_verifier_refuses_a_tampered_payload() {
        let secret = "signing-secret";
        let token = sign(
            secret,
            &json!({"tenant": "tenant:acme", "scopes": ["operator"]}),
        );

        // Rewrite the claims to grant the platform scope, keeping the original
        // signature: it no longer covers the body it is attached to.
        let parts: Vec<&str> = token.split('.').collect();
        let mut claims: serde_json::Value =
            serde_json::from_slice(&b64url_decode(parts[1]).expect("payload")).expect("claims");
        claims["scopes"] = json!(["platform", "operator"]);
        let tampered = format!(
            "{}.{}.{}",
            parts[0],
            b64url_encode(&serde_json::to_vec(&claims).expect("re-encode")),
            parts[2]
        );

        assert!(JwtPlatformVerifier::new(secret).verify(&tampered).is_err());
    }

    /// `alg: "none"` is the oldest JWT forgery there is: drop the signature,
    /// keep whatever payload you like, and hope the verifier honors the header's
    /// choice of algorithm. `Validation::new(Algorithm::HS256)` pins the
    /// algorithm so it does not, and clearing `required_spec_claims` does not
    /// loosen that — but only a test says so out loud. The same claims, signed,
    /// are accepted, which leaves the header as the only thing the rejection can
    /// be about.
    #[cfg(feature = "platform-jwt")]
    #[test]
    fn jwt_verifier_refuses_an_unsigned_token() {
        let secret = "signing-secret";
        let claims = json!({"tenant": "tenant:victim", "scopes": ["platform", "operator"]});

        // Assembled literally, from nothing but the wire shape: header, payload,
        // and the empty signature an `alg: none` token carries.
        let unsigned = format!(
            "{}.{}.",
            b64url_encode(br#"{"alg":"none","typ":"JWT"}"#),
            b64url_encode(&serde_json::to_vec(&claims).expect("claims"))
        );

        assert!(
            JwtPlatformVerifier::new(secret).verify(&unsigned).is_err(),
            "an unsigned `alg: none` token must not resolve to platform claims"
        );

        let signed = sign(secret, &claims);
        assert!(JwtPlatformVerifier::new(secret).verify(&signed).is_ok());
    }

    /// [`constant_time_eq`] must agree with `==` on every outcome — the
    /// property it changes is timing, never which strings compare equal.
    #[test]
    fn constant_time_eq_matches_ordinary_string_equality() {
        assert!(constant_time_eq("", ""));
        assert!(constant_time_eq("top-secret", "top-secret"));
        assert!(!constant_time_eq("top-secret", ""));
        assert!(!constant_time_eq("", "top-secret"));
        // Differing length, shorter and longer than the reference.
        assert!(!constant_time_eq("top-secret", "top-secre"));
        assert!(!constant_time_eq("top-secret", "top-secrets"));
        // Same length, differing at the first byte, the last byte, and the
        // middle — a short-circuiting compare returns at different points for
        // each of these, so a constant-time one must not accidentally special
        // case any of them.
        assert!(!constant_time_eq("top-secret", "xop-secret"));
        assert!(!constant_time_eq("top-secret", "top-secreX"));
        assert!(!constant_time_eq("top-secret", "top-Xecret"));
        // Every byte differs.
        assert!(!constant_time_eq("aaaa", "zzzz"));
    }

    #[test]
    fn unrecognized_token_is_rejected() {
        let verifier = StaticPlatformVerifier::new("top-secret");
        assert!(verifier.verify("nope").is_err());
        assert!(verifier.verify("oc_tenant.@@@not-base64@@@").is_err());
    }

    #[test]
    fn no_credential_configures_no_platform_auth() {
        assert!(configure(None, None).unwrap().is_none());
        // A blank injected variable is unset, not an empty accepted bearer.
        assert!(
            configure(Some("  ".to_string()), Some(String::new()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn shared_secret_alone_configures_shared_secret_mode() {
        let (config, mode) = configure(Some("plat-secret".to_string()), None)
            .unwrap()
            .unwrap();
        assert_eq!(mode, PlatformAuthMode::SharedSecret);
        assert!(
            config
                .verifier
                .verify("plat-secret")
                .unwrap()
                .has_platform_scope()
        );
        assert!(config.verifier.verify("wrong").is_err());
    }

    #[cfg(feature = "platform-jwt")]
    #[test]
    fn signing_secret_alone_configures_jwt_mode() {
        let (config, mode) = configure(None, Some("signing-secret".to_string()))
            .unwrap()
            .unwrap();
        assert_eq!(mode, PlatformAuthMode::Jwt);

        let token = sign(
            "signing-secret",
            &json!({"tenant": "tenant:acme", "scopes": ["operator"]}),
        );
        assert_eq!(
            config.verifier.verify(&token).unwrap().tenant,
            "tenant:acme"
        );
        // The shared-secret arm is not wired, so nothing else gets in.
        assert!(config.verifier.verify("signing-secret").is_err());
    }

    #[cfg(feature = "platform-jwt")]
    #[test]
    fn both_secrets_accept_either_credential() {
        let (config, mode) = configure(
            Some("plat-secret".to_string()),
            Some("signing-secret".to_string()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(mode, PlatformAuthMode::Both);

        assert!(
            config
                .verifier
                .verify("plat-secret")
                .unwrap()
                .has_platform_scope()
        );
        let token = sign(
            "signing-secret",
            &json!({"tenant": "tenant:acme", "scopes": ["operator"]}),
        );
        assert_eq!(
            config.verifier.verify(&token).unwrap().tenant,
            "tenant:acme"
        );

        // And a bearer neither arm authenticates is still refused.
        assert!(config.verifier.verify("oc_tenant.e30").is_err());
        assert!(config.verifier.verify("nope").is_err());
    }

    /// A build that cannot verify signatures must abort boot when a signing
    /// secret is set, not fall back to the shared secret. `configure_with` takes
    /// the availability flag so this stays observable on a build that *has* the
    /// feature; a featureless build reaches the same error through
    /// `jwt_verifier`.
    #[test]
    fn signing_secret_without_the_feature_refuses_to_boot() {
        let err = configure_with(
            Some("plat-secret".to_string()),
            Some("signing-secret".to_string()),
            false,
        )
        .expect_err("a signing secret this build cannot use must abort boot");

        let message = err.to_string();
        assert!(
            message.contains(PLATFORM_JWT_SECRET_ENV) && message.contains("platform-jwt"),
            "the refusal must name the variable and the missing feature: {message}"
        );
        assert!(
            !message.contains("signing-secret"),
            "the refusal must never carry secret material: {message}"
        );
    }

    #[test]
    fn auth_mode_names_are_stable() {
        assert_eq!(PlatformAuthMode::SharedSecret.to_string(), "shared-secret");
        assert_eq!(PlatformAuthMode::Jwt.to_string(), "jwt");
        assert_eq!(PlatformAuthMode::Both.to_string(), "both");
    }

    #[test]
    fn may_address_honors_allow_list() {
        let mut claims = tenant_claims("tenant:acme", &["operator"]);
        assert!(claims.may_address(&CompanyId::new("anything")));
        claims.companies = Some(HashSet::from(["acme".to_string()]));
        assert!(claims.may_address(&CompanyId::new("acme")));
        assert!(!claims.may_address(&CompanyId::new("globex")));
    }

    /// [`CompanyAuth`] only authenticates: it resolves *a* principal, not
    /// whether that principal may reach the addressed company.
    /// [`authorize_address`] is the separate call every real handler makes
    /// right after — this is the one place that pairing is proven directly
    /// against the function itself, rather than only through whichever route
    /// handler happens to call it. A tenant token that owns a *different*
    /// company must be refused `403`, not let through because it merely
    /// verified.
    #[test]
    fn authorize_address_denies_a_platform_token_for_a_company_it_does_not_own() {
        use crate::app::AppConfig;

        let state = crate::AppState::new(AppConfig::default());
        state.set_owner(CompanyId::new("globex"), "tenant:globex-corp");

        let auth = GqlAuth::Platform(tenant_claims("tenant:acme-corp", &["operator"]));
        let resp = authorize_address(&state, &auth, &CompanyId::new("globex"))
            .expect("a tenant that does not own the addressed company must be refused");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    /// The positive control for the test above: the same shape, but the
    /// tenant actually owns the company, so `authorize_address` must let it
    /// through (`None`). Without this, the denial test could pass for the
    /// wrong reason (e.g. every call refused).
    #[test]
    fn authorize_address_allows_a_platform_token_for_a_company_it_owns() {
        use crate::app::AppConfig;

        let state = crate::AppState::new(AppConfig::default());
        state.set_owner(CompanyId::new("acme"), "tenant:acme-corp");

        let auth = GqlAuth::Platform(tenant_claims("tenant:acme-corp", &["operator"]));
        assert!(authorize_address(&state, &auth, &CompanyId::new("acme")).is_none());
    }

    /// The `platform` scope is not tenant-owned at all — it is the hosting
    /// layer's own credential and may address any company, including one no
    /// tenant owns yet (e.g. mid-provisioning). Distinct from the allow-list
    /// check: platform scope bypasses ownership entirely.
    #[test]
    fn authorize_address_platform_scope_bypasses_ownership() {
        use crate::app::AppConfig;

        let state = crate::AppState::new(AppConfig::default());
        // Deliberately unowned.
        let auth = GqlAuth::Platform(tenant_claims("tenant:platform", &[SCOPE_PLATFORM]));
        assert!(authorize_address(&state, &auth, &CompanyId::new("unowned")).is_none());
    }

    /// Ownership alone is not enough: a tenant token whose own claims carry an
    /// allow-list that excludes the company must still be refused, even
    /// though the ownership map says the tenant owns it. Two independent
    /// checks — [`AppState::owner_of`] and [`PlatformClaims::may_address`] —
    /// both have to say yes.
    #[test]
    fn authorize_address_honors_the_claims_allow_list_even_when_the_tenant_owns_the_company() {
        use crate::app::AppConfig;

        let state = crate::AppState::new(AppConfig::default());
        state.set_owner(CompanyId::new("acme"), "tenant:acme-corp");

        let mut claims = tenant_claims("tenant:acme-corp", &["operator"]);
        claims.companies = Some(HashSet::from(["some-other-company".to_string()]));
        let auth = GqlAuth::Platform(claims);

        let resp = authorize_address(&state, &auth, &CompanyId::new("acme"))
            .expect("an allow-list that excludes the company must refuse even the owning tenant");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    /// A user session is scoped to exactly one company by construction
    /// ([`UserPrincipal::company`]); `authorize_address` refuses any other.
    /// There is no ownership map involved on this arm at all — a session
    /// minted for one company must never authorize a request against
    /// another, however the ids happen to be spelled.
    #[test]
    fn authorize_address_denies_a_user_session_addressing_a_different_company() {
        use crate::app::AppConfig;
        use crate::ports::SessionKind;
        use crate::ports::users::UserRole;
        use crate::server::graphql::auth::UserPrincipal;

        let state = crate::AppState::new(AppConfig::default());
        let auth = GqlAuth::User(UserPrincipal {
            company: CompanyId::new("acme"),
            user_id: "u1".to_string(),
            email: "a@example.test".to_string(),
            role: UserRole::Admin,
            must_change_password: false,
            session_token_hash: "hash".to_string(),
            credential: SessionKind::Browser,
        });

        let resp = authorize_address(&state, &auth, &CompanyId::new("globex"))
            .expect("a session minted for one company must not authorize another");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(authorize_address(&state, &auth, &CompanyId::new("acme")).is_none());
    }

    /// [`CompanyAuth`] over a route with **no guard of its own** — the shape a
    /// new route acquires by default, and the shape that used to be
    /// cross-company-open.
    ///
    /// Every existing handler pairs the extractor with [`authorize_address`],
    /// so none of these are reachable defects today; they are reachable the
    /// moment somebody writes the obvious handler. Driving a deliberately
    /// unguarded route is the only way to assert what the *extractor* decides
    /// rather than what one handler remembered to ask.
    mod addressed {
        use std::sync::Arc;

        use axum::body::Body;
        use axum::extract::Path;
        use axum::http::Request;
        use axum::routing::get;
        use axum::{Router, http::StatusCode};
        use tower::ServiceExt;

        use super::super::CompanyAuth;
        use crate::AppState;
        use crate::company::CompanyManifest;
        use crate::ports::types::CompanyId;
        use crate::runtime::RuntimeBuilder;
        use crate::server::test_support;

        /// The unguarded handler: a principal and a company id, and nothing
        /// asking whether the one may address the other.
        async fn probe(CompanyAuth(_auth): CompanyAuth, Path(id): Path<String>) -> String {
            id
        }

        /// The alias shape — no `{id}` at all — so the addressed check can be
        /// shown to fire only when the path names a company.
        async fn probe_alias(CompanyAuth(_auth): CompanyAuth) -> &'static str {
            "sole"
        }

        /// Two companies under two tenants: `acme` owned by `tenant:matrix-owner`
        /// (the [`FIXED_TENANT_OWNER_TEST_TOKEN`](test_support::FIXED_TENANT_OWNER_TEST_TOKEN)
        /// bearer) and `globex` owned by the other fixed tenant. One company
        /// cannot tell "refused correctly" from "there was nothing to reach".
        async fn state_with_two_tenants(home: &std::path::Path) -> AppState {
            let manifest: CompanyManifest =
                toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap();
            let state = AppState::new(crate::AppConfig::default())
                .with_home(home.to_path_buf())
                .with_platform_auth(test_support::fixed_principal_platform_auth());
            for name in ["acme", "globex"] {
                let id = CompanyId::new(name);
                let runtime = RuntimeBuilder::new(home.to_path_buf(), manifest.clone())
                    .with_id(id.clone())
                    .build()
                    .await
                    .unwrap();
                state.registry().insert(id, Arc::new(runtime));
                test_support::seed_fixed_admin(&state, name).await;
            }
            state.set_owner(CompanyId::new("acme"), "tenant:matrix-owner");
            state.set_owner(CompanyId::new("globex"), "tenant:matrix-outsider");
            state
        }

        fn app(state: AppState) -> Router {
            Router::new()
                .route("/api/v1/companies/{id}/probe", get(probe))
                .route("/api/v1/probe", get(probe_alias))
                .with_state(state)
        }

        async fn reach(app: &Router, uri: &str, bearer: Option<&str>) -> StatusCode {
            let mut request = Request::builder().uri(uri);
            if let Some(bearer) = bearer {
                request = request.header("authorization", format!("Bearer {bearer}"));
            }
            app.clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
        }

        /// AUTH. A verified tenant credential is not a licence to address every
        /// company on the host: the one it owns is served, the one it does not
        /// is `403`.
        #[tokio::test]
        async fn an_unguarded_route_refuses_a_tenant_the_company_it_does_not_own() {
            let home = tempfile::tempdir().unwrap();
            let app = app(state_with_two_tenants(home.path()).await);
            let owner = test_support::FIXED_TENANT_OWNER_TEST_TOKEN;

            assert_eq!(
                reach(&app, "/api/v1/companies/acme/probe", Some(owner)).await,
                StatusCode::OK,
                "the owning tenant must still be served its own company"
            );
            assert_eq!(
                reach(&app, "/api/v1/companies/globex/probe", Some(owner)).await,
                StatusCode::FORBIDDEN,
                "a route that asked nothing served a tenant another tenant's company"
            );
        }

        /// INPUT. The `{id}` segment — not the credential — names the target,
        /// so the refusal has to follow it in both directions rather than
        /// hard-coding one tenant as the outsider.
        #[tokio::test]
        async fn the_path_id_is_what_decides_which_company_is_refused() {
            let home = tempfile::tempdir().unwrap();
            let app = app(state_with_two_tenants(home.path()).await);
            let outsider = test_support::FIXED_TENANT_NON_OWNER_TEST_TOKEN;

            assert_eq!(
                reach(&app, "/api/v1/companies/globex/probe", Some(outsider)).await,
                StatusCode::OK,
                "the second tenant owns globex"
            );
            assert_eq!(
                reach(&app, "/api/v1/companies/acme/probe", Some(outsider)).await,
                StatusCode::FORBIDDEN,
                "and must not reach the first tenant's company"
            );
        }

        /// STATE. Ownership is read from the registry on every request, not
        /// captured when the credential was minted — a tenant that acquires a
        /// company reaches it immediately, and one that loses it stops.
        #[tokio::test]
        async fn ownership_is_re_read_on_every_request() {
            let home = tempfile::tempdir().unwrap();
            let state = state_with_two_tenants(home.path()).await;
            let app = app(state.clone());
            let outsider = test_support::FIXED_TENANT_NON_OWNER_TEST_TOKEN;

            assert_eq!(
                reach(&app, "/api/v1/companies/acme/probe", Some(outsider)).await,
                StatusCode::FORBIDDEN
            );
            state.set_owner(CompanyId::new("acme"), "tenant:matrix-outsider");
            assert_eq!(
                reach(&app, "/api/v1/companies/acme/probe", Some(outsider)).await,
                StatusCode::OK,
                "the ownership map moved; the extractor must have re-read it"
            );
        }

        /// BOUND. Two edges of the check. No credential at all stays `401` —
        /// authorization must not restate an unauthenticated request as a role
        /// decision — and the alias form, which names no company in its path,
        /// is untouched by it.
        #[tokio::test]
        async fn no_credential_is_unauthorized_and_the_alias_form_is_untouched() {
            let home = tempfile::tempdir().unwrap();
            let app = app(state_with_two_tenants(home.path()).await);

            assert_eq!(
                reach(&app, "/api/v1/companies/acme/probe", None).await,
                StatusCode::UNAUTHORIZED,
                "an anonymous request is not a forbidden one"
            );
            assert_eq!(
                reach(&app, "/api/v1/companies/acme/probe", Some("not-a-token")).await,
                StatusCode::UNAUTHORIZED,
                "and neither is an unverifiable one"
            );
            assert_eq!(
                reach(
                    &app,
                    "/api/v1/probe",
                    Some(test_support::FIXED_TENANT_NON_OWNER_TEST_TOKEN)
                )
                .await,
                StatusCode::OK,
                "the alias form names no company, so the addressed check must not fire"
            );
        }
    }

    #[cfg(feature = "platform-jwt")]
    #[test]
    fn jwt_verifier_round_trips_signed_claims() {
        use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};

        let secret = "signing-secret";
        let claims = tenant_claims("tenant:acme", &["platform", "operator"]);
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        let verifier = JwtPlatformVerifier::new(secret);
        let verified = verifier.verify(&token).unwrap();
        assert_eq!(verified.tenant, "tenant:acme");
        assert!(verified.has_platform_scope());

        // A token signed with the wrong secret is rejected.
        let wrong = JwtPlatformVerifier::new("other-secret");
        assert!(wrong.verify(&token).is_err());
    }
}
