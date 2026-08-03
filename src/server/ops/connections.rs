//! OAuth connection lifecycle: start, callback, disconnect.
//!
//! Gated behind the `oauth` feature because token exchange needs `reqwest`;
//! without it these routes are absent (404) and the console shows the read-only
//! connections catalog. Provider **app** credentials (client id/secret) are
//! host-level configuration read from the environment
//! (`OPENCOMPANY_OAUTH_<PROVIDER>_ID` / `_SECRET`); per-company state is tokens
//! only, and those live in [`SecretStore`](crate::ports::SecretStore) under
//! `oauth/{provider}` — token material never appears in any response.
//!
//! The authorize URL carries a signed `state` nonce binding the flow to one
//! company + provider + expiry, verified on callback so a tampered `state` is
//! refused before any token exchange.
//!
//! The callback is reached by a **browser navigation**, so it never answers with
//! a body: every outcome — success, cancellation, a bad nonce, a failed exchange
//! — redirects to the console's Connections view carrying a stable, non-secret
//! outcome code. Returning JSON there strands the operator on a blank page with
//! no route back (issue #300).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::Uri;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::company::runtime::CompanyRuntime;
use crate::error::OpenCompanyError;
use crate::ports::now_millis;
use crate::ports::types::{CompanyId, SecretValue};
use crate::server::error::ApiError;
use crate::server::ops::{ScopedCompany, oauth_key, scoped};
use crate::server::webhook::{DefaultHashSigner, WebhookSigner};

/// How long a signed `state` nonce stays valid.
const STATE_TTL_MS: u64 = 10 * 60 * 1000;

/// Builds the OAuth route fragment.
pub fn router() -> Router<AppState> {
    scoped("/connections/{provider}/start", post(start))
        .merge(scoped(
            "/connections/{provider}/disconnect",
            post(disconnect),
        ))
        // The callback is unscoped: the signed `state` carries the company id.
        .route("/api/v1/oauth/callback", get(callback))
}

/// The provider sub-resource path (`provider`); the scope `id` is consumed by
/// the [`ScopedCompany`] extractor.
#[derive(Debug, Deserialize)]
struct ProviderPath {
    provider: String,
}

// ---------------------------------------------------------------------------
// Provider app configuration (host-level env)
// ---------------------------------------------------------------------------

/// A provider's OAuth endpoints and host-level app credentials.
struct ProviderConfig {
    client_id: String,
    client_secret: String,
    authorize_url: String,
    token_url: String,
    default_scopes: String,
}

/// Well-known authorize/token URLs for the built-in providers; overridable per
/// provider via `OPENCOMPANY_OAUTH_<P>_AUTHORIZE_URL` / `_TOKEN_URL`.
fn well_known(provider: &str) -> Option<(&'static str, &'static str)> {
    match provider {
        "slack" => Some((
            "https://slack.com/oauth/v2/authorize",
            "https://slack.com/api/oauth.v2.access",
        )),
        "google" | "gmail" => Some((
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
        )),
        "github" => Some((
            "https://github.com/login/oauth/authorize",
            "https://github.com/login/oauth/access_token",
        )),
        _ => None,
    }
}

/// Resolves a provider's config from the environment, or `None` when the app
/// credentials are not configured (the provider is not enabled on this host).
fn provider_config(provider: &str) -> Option<ProviderConfig> {
    let key = provider.to_ascii_uppercase();
    let env = |suffix: &str| std::env::var(format!("OPENCOMPANY_OAUTH_{key}_{suffix}")).ok();
    let client_id = env("ID")?;
    let client_secret = env("SECRET")?;
    let (default_authorize, default_token) = well_known(provider).unwrap_or(("", ""));
    let authorize_url = env("AUTHORIZE_URL").unwrap_or_else(|| default_authorize.to_string());
    let token_url = env("TOKEN_URL").unwrap_or_else(|| default_token.to_string());
    if authorize_url.is_empty() || token_url.is_empty() {
        return None;
    }
    Some(ProviderConfig {
        client_id,
        client_secret,
        authorize_url,
        token_url,
        default_scopes: env("SCOPES").unwrap_or_default(),
    })
}

/// The redirect URI advertised to the provider. `OPENCOMPANY_OAUTH_REDIRECT_BASE`
/// overrides the origin so the authorize URL points where the operator's
/// browser can reach the callback (managed deployments front it via the manager).
fn redirect_uri(state: &AppState) -> String {
    let base = std::env::var("OPENCOMPANY_OAUTH_REDIRECT_BASE")
        .unwrap_or_else(|_| state.config().host_base_url());
    format!("{}/api/v1/oauth/callback", base.trim_end_matches('/'))
}

/// The host-level secret the `state` nonce is signed with.
fn state_secret() -> String {
    std::env::var("OPENCOMPANY_OAUTH_STATE_SECRET")
        .unwrap_or_else(|_| "opencompany-oauth-state".to_string())
}

// ---------------------------------------------------------------------------
// Console bounce-back
// ---------------------------------------------------------------------------

/// The console origin the callback bounces the operator's browser back to.
/// `OPENCOMPANY_CONSOLE_URL` overrides it for deployments that serve the console
/// from a different origin than the API.
fn console_base(state: &AppState) -> String {
    let base =
        std::env::var("OPENCOMPANY_CONSOLE_URL").unwrap_or_else(|_| state.config().host_base_url());
    base.trim_end_matches('/').to_string()
}

/// Sends the operator's browser back to the console's Connections view.
///
/// **Every** callback exit goes through here, success and failure alike. A
/// failure arm that renders a JSON body into the document instead leaves the
/// operator staring at raw error text on a blank page with no route back — the
/// dead end reported in issue #300. A redirect keeps them in the console, where
/// the Connections view can explain what happened and offer a retry.
fn console_redirect(state: &AppState, params: &str) -> Response {
    Redirect::to(&format!("{}/connections?{params}", console_base(state))).into_response()
}

/// Bounces back with a stable, non-secret failure code.
///
/// The code is one of a closed set the console maps to operator-facing copy.
/// The provider's own error text is deliberately **not** propagated: it is
/// attacker-influenced, unbounded, and may carry credential material — none of
/// which belongs in a URL that lands in browser history and server access logs.
///
/// `provider` is optional because the arms that fire before the signed `state`
/// is verified cannot always know which provider the flow was for.
fn connect_error(state: &AppState, code: &str, provider: Option<&str>) -> Response {
    let mut params = format!("connect_error={}", urlencode(code));
    if let Some(provider) = provider {
        params.push_str(&format!("&provider={}", urlencode(provider)));
    }
    console_redirect(state, &params)
}

// ---------------------------------------------------------------------------
// Signed state nonce
// ---------------------------------------------------------------------------

/// Encodes `company:provider:exp:sig` into an opaque `state` value.
fn encode_state(company: &str, provider: &str, exp: u64) -> String {
    let payload = format!("{company}:{provider}:{exp}");
    let sig = DefaultHashSigner.sign(&state_secret(), payload.as_bytes());
    format!("{payload}:{sig}")
}

/// Verifies and decodes a `state` value into `(company, provider)`, or `None`
/// when the signature is wrong or the nonce has expired.
fn decode_state(state: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = state.splitn(4, ':').collect();
    if parts.len() != 4 {
        return None;
    }
    let (company, provider, exp, sig) = (parts[0], parts[1], parts[2], parts[3]);
    let payload = format!("{company}:{provider}:{exp}");
    let expected = DefaultHashSigner.sign(&state_secret(), payload.as_bytes());
    if sig != expected {
        return None;
    }
    let exp: u64 = exp.parse().ok()?;
    if now_millis() > exp {
        return None;
    }
    Some((company.to_string(), provider.to_string()))
}

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

/// The authorize-URL response.
#[derive(Debug, Serialize)]
struct StartResponse {
    /// The provider authorize URL the operator's browser should visit.
    url: String,
}

/// Builds the authorize URL for `provider` scoped to `company`.
fn build_authorize(
    state: &AppState,
    company: &CompanyId,
    provider: &str,
) -> Result<StartResponse, ApiError> {
    let Some(config) = provider_config(provider) else {
        return Err(ApiError(OpenCompanyError::InvalidRequest(format!(
            "provider '{provider}' is not enabled on this host"
        ))));
    };
    let nonce = encode_state(company.as_ref(), provider, now_millis() + STATE_TTL_MS);
    let redirect = redirect_uri(state);
    let url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}",
        config.authorize_url,
        urlencode(&config.client_id),
        urlencode(&redirect),
        urlencode(&config.default_scopes),
        urlencode(&nonce),
    );
    Ok(StartResponse { url })
}

/// `POST …/connections/{provider}/start` (both scope forms).
async fn start(
    company: ScopedCompany,
    State(state): State<AppState>,
    Path(ProviderPath { provider }): Path<ProviderPath>,
) -> Result<Json<StartResponse>, ApiError> {
    Ok(Json(build_authorize(&state, company.id(), &provider)?))
}

// ---------------------------------------------------------------------------
// Callback
// ---------------------------------------------------------------------------

/// Reads a single query parameter from a raw query string, percent-decoding it.
fn query_param(uri: &Uri, key: &str) -> Option<String> {
    let query = uri.query()?;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k == key
        {
            return Some(percent_decode(v));
        }
    }
    None
}

/// Minimal percent-decode (and `+` → space) for query values.
fn percent_decode(value: &str) -> String {
    let bytes = value.replace('+', " ");
    let bytes = bytes.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&value[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The provider a callback was for, recovered from a **signature-verified**
/// `state` — used only to enrich a failure bounce-back with the provider name.
/// An unverified `state` is never trusted for this, so a tampered value simply
/// yields `None` and the console shows the provider-less message.
fn provider_from_state(uri: &Uri) -> Option<String> {
    let raw = query_param(uri, "state")?;
    decode_state(&raw).map(|(_, provider)| provider)
}

/// `GET /api/v1/oauth/callback` — verify state, exchange code, store tokens.
///
/// Always answers with a redirect into the console (see [`console_redirect`]):
/// this route is reached by a **browser navigation**, so any body it returns is
/// rendered as the page the operator is left on.
async fn callback(State(state): State<AppState>, uri: Uri) -> Response {
    if let Some(err) = query_param(&uri, "error") {
        // Log the provider's text host-side for diagnosis; never forward it to
        // the browser (see `connect_error`).
        tracing::warn!(provider_error = %err, "oauth callback: provider returned an error");
        return connect_error(&state, "denied", provider_from_state(&uri).as_deref());
    }
    let (Some(code), Some(raw_state)) = (query_param(&uri, "code"), query_param(&uri, "state"))
    else {
        return connect_error(
            &state,
            "invalid_request",
            provider_from_state(&uri).as_deref(),
        );
    };
    // A tampered or expired `state` is rejected before any exchange.
    let Some((company, provider)) = decode_state(&raw_state) else {
        // No provider: the only claim to one came from the `state` we just
        // refused to trust.
        return connect_error(&state, "invalid_state", None);
    };
    let Some(runtime) = state.registry().get(&CompanyId::new(&company)) else {
        return connect_error(&state, "unknown_company", Some(&provider));
    };
    let Some(config) = provider_config(&provider) else {
        return connect_error(&state, "provider_disabled", Some(&provider));
    };

    match exchange_code(&state, &config, &code).await {
        Ok(token_json) => {
            let account = extract_account(&token_json);
            let stored = json!({ "token": token_json, "account": account });
            if let Err(err) = runtime
                .secrets()
                .set(
                    runtime.id(),
                    &oauth_key(&provider),
                    SecretValue(stored.to_string()),
                )
                .await
            {
                tracing::warn!(provider, "oauth callback: storing the token failed: {err}");
                return connect_error(&state, "store_failed", Some(&provider));
            }
            // Redirect the browser back to the console connections view.
            console_redirect(&state, &format!("connected={}", urlencode(&provider)))
        }
        Err(err) => {
            tracing::warn!(provider, "oauth callback: token exchange failed: {err}");
            connect_error(&state, "exchange_failed", Some(&provider))
        }
    }
}

/// Exchanges an authorization code for tokens at the provider's token endpoint.
async fn exchange_code(
    state: &AppState,
    config: &ProviderConfig,
    code: &str,
) -> Result<serde_json::Value, OpenCompanyError> {
    let client = reqwest::Client::new();
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("client_id", config.client_id.as_str()),
        ("client_secret", config.client_secret.as_str()),
        ("redirect_uri", &redirect_uri(state)),
    ];
    let resp = client
        .post(&config.token_url)
        .header("Accept", "application/json")
        .form(&params)
        .send()
        .await
        .map_err(|e| OpenCompanyError::Store(format!("oauth token exchange failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(OpenCompanyError::Store(format!(
            "oauth token endpoint returned {}",
            resp.status()
        )));
    }
    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| OpenCompanyError::Store(format!("oauth token response not JSON: {e}")))
}

/// Extracts a human-friendly account label from a token response, if present.
fn extract_account(token: &serde_json::Value) -> Option<String> {
    for key in ["account", "email", "login", "user_login"] {
        if let Some(value) = token.get(key).and_then(|v| v.as_str()) {
            return Some(value.to_string());
        }
    }
    // Slack nests the workspace under `team.name`.
    token
        .get("team")
        .and_then(|team| team.get("name"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Disconnect
// ---------------------------------------------------------------------------

/// The provider's token-revocation endpoint, if one is known. GitHub's URL
/// carries the app `client_id` in its path. Overridable per provider via
/// `OPENCOMPANY_OAUTH_<P>_REVOKE_URL` (tests point this at a local mock).
/// `None` means "no known revoke flow" — disconnect still blanks the local
/// secret; there is simply no remote call to make.
fn revoke_url(provider: &str, config: &ProviderConfig) -> Option<String> {
    let key = provider.to_ascii_uppercase();
    if let Some(url) = std::env::var(format!("OPENCOMPANY_OAUTH_{key}_REVOKE_URL"))
        .ok()
        .filter(|u| !u.is_empty())
    {
        return Some(url);
    }
    match provider {
        "slack" => Some("https://slack.com/api/auth.revoke".to_string()),
        "github" => Some(format!(
            "https://api.github.com/applications/{}/grant",
            config.client_id
        )),
        _ => None,
    }
}

/// Reads the stored `access_token` for a provider, if a non-empty token blob is
/// present. Returns `None` when nothing is stored or the blob has no token.
/// The token is used only to build the revoke request and is never logged.
async fn stored_access_token(runtime: &CompanyRuntime, provider: &str) -> Option<String> {
    let value = runtime
        .secrets()
        .get(runtime.id(), &oauth_key(provider))
        .await
        .ok()
        .flatten()?;
    if value.expose().trim().is_empty() {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(value.expose())
        .ok()?
        .get("token")
        .and_then(|t| t.get("access_token"))
        .and_then(|a| a.as_str())
        .map(str::to_string)
}

/// Best-effort provider-side token revocation, run before the local secret is
/// blanked. Any failure — unknown provider, unconfigured app credentials, a
/// network error, or a non-success status — is logged and swallowed so the
/// disconnect always proceeds. Token material is never logged or returned.
async fn best_effort_revoke(runtime: &CompanyRuntime, provider: &str) {
    let Some(access_token) = stored_access_token(runtime, provider).await else {
        return;
    };
    let Some(config) = provider_config(provider) else {
        return;
    };
    let Some(url) = revoke_url(provider, &config) else {
        return;
    };
    // Bound the best-effort revoke: a provider that accepts the connection but
    // hangs (or a slow `_REVOKE_URL` override) must not block the disconnect
    // handler indefinitely. A builder failure skips the remote revoke entirely
    // — the caller blanks the local secret unconditionally, so this stays
    // best-effort without blocking or panicking.
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            tracing::warn!(provider, "oauth revoke client build failed: {err}");
            return;
        }
    };
    let request = match provider {
        // GitHub revokes a grant with an authenticated DELETE carrying the token
        // in the JSON body and the app credentials as Basic auth.
        "github" => client
            .delete(&url)
            .basic_auth(&config.client_id, Some(&config.client_secret))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "opencompany")
            .json(&json!({ "access_token": access_token })),
        // Slack (and any override that behaves like it) revokes with a POST form.
        _ => client.post(&url).form(&[("token", access_token.as_str())]),
    };
    match request.send().await {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => {
            tracing::warn!(provider, status = %resp.status(), "oauth revoke returned non-success")
        }
        Err(err) => tracing::warn!(provider, "oauth revoke request failed: {err}"),
    }
}

/// Revokes the provider grant (best-effort) then deletes the stored tokens.
async fn do_disconnect(
    runtime: Arc<CompanyRuntime>,
    provider: &str,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Ask the provider to invalidate the grant first, best-effort: a failed or
    // absent revoke must never block the local disconnect below.
    best_effort_revoke(&runtime, provider).await;
    // Overwrite with an empty marker: the secret store has no delete; an empty
    // value reads back as "not connected" on the read side.
    runtime
        .secrets()
        .set(
            runtime.id(),
            &oauth_key(provider),
            SecretValue(String::new()),
        )
        .await?;
    Ok(Json(json!({ "connected": false, "provider": provider })))
}

/// `POST …/connections/{provider}/disconnect` (both scope forms).
async fn disconnect(
    company: ScopedCompany,
    Path(ProviderPath { provider }): Path<ProviderPath>,
) -> Result<Json<serde_json::Value>, ApiError> {
    do_disconnect(company.runtime, &provider).await
}

/// Minimal percent-encoding for URL query values (RFC 3986 unreserved set kept).
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn state_round_trips_and_rejects_tampering() {
        let state = encode_state("acme", "slack", now_millis() + STATE_TTL_MS);
        let (company, provider) = decode_state(&state).expect("valid state");
        assert_eq!(company, "acme");
        assert_eq!(provider, "slack");

        // A tampered signature fails. Flip the last char to a guaranteed-different
        // one so the mutation is never a no-op (a blind `push('0')` leaves the
        // string unchanged — and the assertion flaky — whenever it already ends
        // in '0').
        let mut tampered = state.clone();
        let last = tampered.pop().expect("encoded state is non-empty");
        tampered.push(if last == '0' { '1' } else { '0' });
        assert_ne!(tampered, state, "tamper must actually change the state");
        assert!(decode_state(&tampered).is_none());
    }

    #[test]
    fn expired_state_is_rejected() {
        let state = encode_state("acme", "slack", now_millis().saturating_sub(1));
        assert!(decode_state(&state).is_none());
    }

    #[test]
    fn urlencode_escapes_reserved() {
        assert_eq!(urlencode("a b/c"), "a%20b%2Fc");
        assert_eq!(urlencode("plain-id_1.0~"), "plain-id_1.0~");
    }

    #[test]
    fn extract_account_reads_common_fields() {
        assert_eq!(
            extract_account(&json!({ "email": "ceo@acme.test" })),
            Some("ceo@acme.test".to_string())
        );
        assert_eq!(
            extract_account(&json!({ "team": { "name": "Acme" } })),
            Some("Acme".to_string())
        );
        assert_eq!(extract_account(&json!({ "access_token": "x" })), None);
    }

    // ---- callback bounce-back (issue #300) ---------------------------------

    use crate::AppConfig;

    /// Drives `callback` with a raw query string and returns the response.
    async fn call_callback(state: AppState, query: &str) -> Response {
        let uri: Uri = format!("/api/v1/oauth/callback?{query}")
            .parse()
            .expect("valid uri");
        callback(State(state), uri).await
    }

    /// The `Location` header of a redirect response, asserting it *is* one.
    fn location(resp: &Response) -> String {
        assert!(
            resp.status().is_redirection(),
            "expected a redirect, got {}",
            resp.status()
        );
        resp.headers()
            .get("location")
            .expect("redirect carries a Location")
            .to_str()
            .expect("ascii location")
            .to_string()
    }

    /// Every failure arm must bounce the browser back to the console's
    /// Connections view — never render a JSON body into the document, which is
    /// the dead end issue #300 reports.
    #[tokio::test]
    async fn callback_failures_redirect_to_console_connections() {
        let state = AppState::new(AppConfig::default());
        for query in [
            "error=access_denied",
            "code=abc",  // missing state
            "state=xyz", // missing code
            "code=abc&state=not-a-valid-state",
        ] {
            let resp = call_callback(state.clone(), query).await;
            let loc = location(&resp);
            assert!(
                loc.contains("/connections?"),
                "{query} did not bounce to the console: {loc}"
            );
            assert!(
                loc.contains("connect_error="),
                "{query} carried no failure code: {loc}"
            );
        }
    }

    /// A cancel at the consent screen maps to `denied`, and — because the
    /// `state` is signature-verified — names the provider so the console can say
    /// which tile failed.
    #[tokio::test]
    async fn callback_denied_names_provider_from_verified_state() {
        let state = AppState::new(AppConfig::default());
        let nonce = encode_state("acme", "slack", now_millis() + STATE_TTL_MS);
        let resp = call_callback(
            state,
            &format!("error=access_denied&state={}", urlencode(&nonce)),
        )
        .await;
        let loc = location(&resp);
        assert!(loc.contains("connect_error=denied"), "{loc}");
        assert!(loc.contains("provider=slack"), "{loc}");
        // The provider's own error text is host-side only: it is
        // attacker-influenced and must never ride in a URL.
        assert!(
            !loc.contains("access_denied"),
            "raw provider error leaked into Location: {loc}"
        );
    }

    /// Without a verifiable `state` there is no trustworthy provider claim, so
    /// the bounce-back carries the code alone.
    #[tokio::test]
    async fn callback_denied_without_state_omits_provider() {
        let state = AppState::new(AppConfig::default());
        let resp = call_callback(state, "error=access_denied").await;
        let loc = location(&resp);
        assert!(loc.contains("connect_error=denied"), "{loc}");
        assert!(!loc.contains("provider="), "{loc}");
    }

    /// A tampered `state` must not be mined for a provider name — the only
    /// claim to one came from the value we just refused to trust.
    #[tokio::test]
    async fn callback_tampered_state_reports_invalid_state_only() {
        let state = AppState::new(AppConfig::default());
        let mut nonce = encode_state("acme", "slack", now_millis() + STATE_TTL_MS);
        let last = nonce.pop().expect("non-empty");
        nonce.push(if last == '0' { '1' } else { '0' });
        let resp = call_callback(state, &format!("code=abc&state={}", urlencode(&nonce))).await;
        let loc = location(&resp);
        assert!(loc.contains("connect_error=invalid_state"), "{loc}");
        assert!(!loc.contains("provider="), "{loc}");
    }

    /// An expired nonce is a recoverable state (the operator simply took too
    /// long), so it bounces back rather than dead-ending on a 401 body.
    #[tokio::test]
    async fn callback_expired_state_redirects() {
        let state = AppState::new(AppConfig::default());
        let nonce = encode_state("acme", "slack", now_millis().saturating_sub(1));
        let resp = call_callback(state, &format!("code=abc&state={}", urlencode(&nonce))).await;
        assert!(
            location(&resp).contains("connect_error=invalid_state"),
            "expired state must bounce back"
        );
    }

    /// A signed state for a company this host doesn't run.
    #[tokio::test]
    async fn callback_unknown_company_redirects() {
        let state = AppState::new(AppConfig::default());
        let nonce = encode_state("ghost", "slack", now_millis() + STATE_TTL_MS);
        let resp = call_callback(state, &format!("code=abc&state={}", urlencode(&nonce))).await;
        let loc = location(&resp);
        assert!(loc.contains("connect_error=unknown_company"), "{loc}");
        assert!(loc.contains("provider=slack"), "{loc}");
    }

    /// A registered company, but the provider has no app credentials on this
    /// host — the catalog/backend gap the console shows 11 tiles for.
    #[tokio::test]
    async fn callback_unconfigured_provider_redirects() {
        let (runtime, _home) = test_runtime().await;
        let state = AppState::new(AppConfig::default());
        state.registry().insert(CompanyId::new("acme"), runtime);
        let provider = unique_provider();
        let nonce = encode_state("acme", &provider, now_millis() + STATE_TTL_MS);
        let resp = call_callback(state, &format!("code=abc&state={}", urlencode(&nonce))).await;
        let loc = location(&resp);
        assert!(loc.contains("connect_error=provider_disabled"), "{loc}");
        assert!(loc.contains(&format!("provider={provider}")), "{loc}");
    }

    /// A token endpoint that cannot be reached must bounce back too — and the
    /// authorization code must not ride along into browser history or logs.
    #[tokio::test]
    async fn callback_exchange_failure_redirects_without_leaking_code() {
        // Bind then drop to obtain a port with nothing listening on it.
        let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = dead.local_addr().unwrap();
        drop(dead);

        let (runtime, _home) = test_runtime().await;
        let state = AppState::new(AppConfig::default());
        state.registry().insert(CompanyId::new("acme"), runtime);
        let provider = unique_provider();
        let key = provider.to_ascii_uppercase();
        // SAFETY: unique per-test provider name → no cross-test env collision.
        unsafe {
            std::env::set_var(format!("OPENCOMPANY_OAUTH_{key}_ID"), "cid");
            std::env::set_var(format!("OPENCOMPANY_OAUTH_{key}_SECRET"), "csec");
            std::env::set_var(
                format!("OPENCOMPANY_OAUTH_{key}_AUTHORIZE_URL"),
                "http://x/a",
            );
            std::env::set_var(
                format!("OPENCOMPANY_OAUTH_{key}_TOKEN_URL"),
                format!("http://{dead_addr}/token"),
            );
        }

        let nonce = encode_state("acme", &provider, now_millis() + STATE_TTL_MS);
        let resp = call_callback(
            state,
            &format!("code=CANARY-authz-code&state={}", urlencode(&nonce)),
        )
        .await;
        let loc = location(&resp);
        assert!(loc.contains("connect_error=exchange_failed"), "{loc}");
        assert!(
            !loc.contains("CANARY-authz-code"),
            "authorization code leaked into Location: {loc}"
        );

        unsafe {
            for suffix in ["ID", "SECRET", "AUTHORIZE_URL", "TOKEN_URL"] {
                std::env::remove_var(format!("OPENCOMPANY_OAUTH_{key}_{suffix}"));
            }
        }
    }

    // ---- disconnect / best-effort revoke ----------------------------------

    use crate::company::CompanyManifest;
    use crate::company::runtime::CompanyRuntime;
    use crate::runtime::RuntimeBuilder;

    /// Builds an isolated in-memory company runtime for disconnect tests.
    ///
    /// The caller must hold the returned handle for the life of the test: it
    /// owns the runtime's home directory and removes it on drop.
    async fn test_runtime() -> (Arc<CompanyRuntime>, tempfile::TempDir) {
        let home = tempfile::Builder::new()
            .prefix("oc-disc-")
            .tempdir()
            .expect("tempdir");
        let manifest: CompanyManifest =
            toml::from_str("[company]\nname = \"Acme\"\n[policy]\nmode = \"full\"\n").unwrap();
        let runtime = RuntimeBuilder::new(home.path().to_path_buf(), manifest)
            .with_id(CompanyId::new("acme"))
            .build()
            .await
            .unwrap();
        (Arc::new(runtime), home)
    }

    /// A process-unique provider id (env-key safe) so the env vars each test
    /// sets never collide with a sibling test running in parallel.
    fn unique_provider() -> String {
        format!("revtest{}", crate::ports::generate_id().replace('-', ""))
    }

    async fn store_token(runtime: &CompanyRuntime, provider: &str, access_token: &str) {
        runtime
            .secrets()
            .set(
                runtime.id(),
                &oauth_key(provider),
                SecretValue(
                    json!({ "token": { "access_token": access_token }, "account": "acc" })
                        .to_string(),
                ),
            )
            .await
            .unwrap();
    }

    async fn is_blanked(runtime: &CompanyRuntime, provider: &str) -> bool {
        runtime
            .secrets()
            .get(runtime.id(), &oauth_key(provider))
            .await
            .unwrap()
            .map(|v| v.expose().trim().is_empty())
            .unwrap_or(true)
    }

    /// No app credentials configured → provider_config is `None`, so there is no
    /// remote to revoke, but the disconnect must still blank the local secret.
    #[tokio::test]
    async fn disconnect_blanks_secret_without_revoke_config() {
        let (runtime, _home) = test_runtime().await;
        let provider = unique_provider();
        store_token(&runtime, &provider, "CANARY-should-never-leak").await;

        let resp = do_disconnect(runtime.clone(), &provider).await.unwrap();
        assert_eq!(resp.0["connected"], false);
        assert!(is_blanked(&runtime, &provider).await, "secret not blanked");
    }

    /// The best-effort revoke path is invoked: with app credentials + a revoke
    /// URL configured, disconnect POSTs the token to the provider endpoint AND
    /// blanks the local secret.
    #[tokio::test]
    async fn disconnect_invokes_provider_revoke() {
        use axum::extract::State;
        use axum::routing::post;

        let hits: Arc<tokio::sync::Mutex<Vec<String>>> =
            Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let app = Router::new()
            .route(
                "/revoke",
                post(|State(hits): State<Arc<tokio::sync::Mutex<Vec<String>>>>, body: String| async move {
                    hits.lock().await.push(body);
                    "ok"
                }),
            )
            .with_state(hits.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let (runtime, _home) = test_runtime().await;
        let provider = unique_provider();
        let key = provider.to_ascii_uppercase();
        // SAFETY: unique per-test provider name → no cross-test env collision.
        unsafe {
            std::env::set_var(format!("OPENCOMPANY_OAUTH_{key}_ID"), "cid");
            std::env::set_var(format!("OPENCOMPANY_OAUTH_{key}_SECRET"), "csec");
            std::env::set_var(
                format!("OPENCOMPANY_OAUTH_{key}_AUTHORIZE_URL"),
                "http://x/a",
            );
            std::env::set_var(format!("OPENCOMPANY_OAUTH_{key}_TOKEN_URL"), "http://x/t");
            std::env::set_var(
                format!("OPENCOMPANY_OAUTH_{key}_REVOKE_URL"),
                format!("http://{addr}/revoke"),
            );
        }

        store_token(&runtime, &provider, "CANARY-revoke-me").await;
        let _ = do_disconnect(runtime.clone(), &provider).await.unwrap();

        let received = hits.lock().await;
        assert_eq!(received.len(), 1, "revoke endpoint was not called");
        assert!(
            received[0].contains("CANARY-revoke-me"),
            "revoke request did not carry the stored token"
        );
        drop(received);
        assert!(is_blanked(&runtime, &provider).await, "secret not blanked");

        unsafe {
            for suffix in ["ID", "SECRET", "AUTHORIZE_URL", "TOKEN_URL", "REVOKE_URL"] {
                std::env::remove_var(format!("OPENCOMPANY_OAUTH_{key}_{suffix}"));
            }
        }
    }

    /// A revoke endpoint that refuses the connection must not fail the
    /// disconnect: the local secret is still blanked.
    #[tokio::test]
    async fn disconnect_blanks_secret_when_revoke_fails() {
        // Bind then drop to obtain a port with nothing listening on it.
        let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = dead.local_addr().unwrap();
        drop(dead);

        let (runtime, _home) = test_runtime().await;
        let provider = unique_provider();
        let key = provider.to_ascii_uppercase();
        // SAFETY: unique per-test provider name → no cross-test env collision.
        unsafe {
            std::env::set_var(format!("OPENCOMPANY_OAUTH_{key}_ID"), "cid");
            std::env::set_var(format!("OPENCOMPANY_OAUTH_{key}_SECRET"), "csec");
            std::env::set_var(
                format!("OPENCOMPANY_OAUTH_{key}_AUTHORIZE_URL"),
                "http://x/a",
            );
            std::env::set_var(format!("OPENCOMPANY_OAUTH_{key}_TOKEN_URL"), "http://x/t");
            std::env::set_var(
                format!("OPENCOMPANY_OAUTH_{key}_REVOKE_URL"),
                format!("http://{dead_addr}/revoke"),
            );
        }

        store_token(&runtime, &provider, "CANARY-unreachable").await;
        // Must still succeed even though the revoke POST cannot connect.
        let _ = do_disconnect(runtime.clone(), &provider).await.unwrap();
        assert!(is_blanked(&runtime, &provider).await, "secret not blanked");

        unsafe {
            for suffix in ["ID", "SECRET", "AUTHORIZE_URL", "TOKEN_URL", "REVOKE_URL"] {
                std::env::remove_var(format!("OPENCOMPANY_OAUTH_{key}_{suffix}"));
            }
        }
    }
}
