// "Sign in with tinyhumans" — mint a Composio bearer without pasting a token
// (issue #110, Layer A). The Composio bearer is a tinyhumans PLATFORM session
// JWT; the composio backend derives the tenant entity from the JWT owner. So
// instead of asking the operator to paste one, we drive the tinyhumans backend's
// web sign-in and capture the JWT it hands back, then store it as the company's
// write-only Composio bearer via `PUT …/composio/token`.
//
// The backend contract (tinyhumansai/backend):
//   1. POST {backend}/auth/email/send-link  {email, frontendRedirectUri}
//      `frontendRedirectUri` MUST be in the backend's OAUTH_LOGIN_ALLOWED_DOMAINS
//      allowlist (infra config) — otherwise 400 "redirect origin is not allowed".
//   2. The emailed link hits GET {backend}/auth/email/verify?token=<magic>, which
//      for a web (http) origin 302s to `{origin}/#auth/callback?token=<JWT>`
//      (src/controllers/auth/emailAuth.ts:63 — the JWT rides the URL *fragment*).
//   3. This console, on return, reads that JWT off the fragment and stores it.
//
// Why the fragment matters for isolation: the console's OPERATOR token is read
// from `location.search` (`?token=`, see `src/config.ts`). The Composio bearer
// arrives in `location.hash` (`#auth/callback?token=`). Different carriers that
// physically cannot collide — signing in for Composio never clobbers the
// operator session token, and vice-versa. We additionally gate capture on a
// per-round-trip `sessionStorage` marker so a stray sign-in link is never
// silently adopted as a company's bearer.

import type { OpenCompanyClient } from "@/api/client";
import { setComposioToken } from "@/api/composio";

/** Fired after a captured sign-in stores a bearer, so a mounted `ComposioSection` refreshes. */
export const COMPOSIO_TOKEN_UPDATED_EVENT = "composio:token-updated";

/** sessionStorage key marking that THIS console started a Composio sign-in. */
const PENDING_KEY = "composio.signin.pending";

/** The fragment path the tinyhumans web sign-in redirects back to. */
const CALLBACK_HASH_PATH = "auth/callback";

/** What we remember across the sign-in round-trip (which company awaits a bearer). */
interface Pending {
  company: string | null;
}

function markPending(company: string | null): void {
  try {
    window.sessionStorage.setItem(PENDING_KEY, JSON.stringify({ company } satisfies Pending));
  } catch {
    // sessionStorage unavailable (private mode / disabled) — capture will simply
    // no-op on return. Non-fatal; the paste-token fallback still works.
  }
}

function readPending(): Pending | null {
  try {
    const raw = window.sessionStorage.getItem(PENDING_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<Pending>;
    return { company: typeof parsed.company === "string" ? parsed.company : null };
  } catch {
    return null;
  }
}

function clearPending(): void {
  try {
    window.sessionStorage.removeItem(PENDING_KEY);
  } catch {
    // Best-effort; a stale marker is harmless without a matching callback JWT.
  }
}

/**
 * Start tinyhumans web sign-in to mint a Composio bearer for `company`.
 *
 * Sends a magic link to `email` via the tinyhumans backend and asks it to
 * redirect back to THIS console origin after verification. Records a pending
 * marker so the returning JWT is routed to Composio (and not mistaken for the
 * operator session). Throws with a human message on a rejected request — most
 * commonly a 400 when the console origin is not yet in the backend's
 * `OAUTH_LOGIN_ALLOWED_DOMAINS` (infra config), or a 502 when the backend has no
 * mail transport configured.
 */
export async function beginComposioSignin(
  backendUrl: string,
  company: string | null,
  email: string,
): Promise<void> {
  const base = backendUrl.trim().replace(/\/+$/, "");
  if (!base) throw new Error("no tinyhumans backend URL is configured for this company");
  let res: Response;
  try {
    res = await fetch(`${base}/auth/email/send-link`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      // No credentials: send-link is unauthenticated and cross-origin; a cookie
      // would only trigger stricter CORS for no gain.
      body: JSON.stringify({ email: email.trim(), frontendRedirectUri: window.location.origin }),
    });
  } catch {
    throw new Error("couldn't reach the tinyhumans backend to start sign-in");
  }
  if (!res.ok) {
    let message = `sign-in request failed (${res.status})`;
    try {
      const body: unknown = await res.json();
      if (body && typeof body === "object" && "error" in body) {
        const err = (body as { error: unknown }).error;
        if (typeof err === "string" && err.trim()) message = err;
      }
    } catch {
      // Non-JSON body — keep the status-code message.
    }
    throw new Error(message);
  }
  markPending(company);
}

/**
 * Read a tinyhumans platform JWT off the sign-in callback fragment
 * (`{origin}/#auth/callback?token=<JWT>`).
 *
 * **Pure** — no side effects, safe under StrictMode's double render. Returns the
 * token, or `null` when the fragment is not a sign-in callback / carries none.
 */
export function readComposioCallbackJwt(): string | null {
  // location.hash is e.g. "#auth/callback?token=<JWT>"; strip the leading "#/"?.
  const hash = window.location.hash.replace(/^#\/?/, "");
  const qIdx = hash.indexOf("?");
  if (qIdx === -1) return null;
  if (hash.slice(0, qIdx) !== CALLBACK_HASH_PATH) return null;
  const token = new URLSearchParams(hash.slice(qIdx + 1)).get("token");
  return token && token.trim() ? token.trim() : null;
}

/**
 * Strip the sign-in callback out of the address bar so the JWT never lingers in
 * the URL or history, and land the operator on the Connections view (where they
 * started). Leaves `location.search` — including any operator `?token=` — intact.
 */
function clearCallbackFromUrl(): void {
  const { pathname, search } = window.location;
  window.history.replaceState({}, "", `${pathname}${search}#/connections`);
}

/** The outcome of a captured sign-in. */
export interface ComposioCaptureResult {
  company: string | null;
}

/**
 * Consume a returning sign-in: if the fragment carries a JWT AND this console
 * started a Composio sign-in, store that JWT as the company's write-only
 * Composio bearer. Idempotent — it clears the pending marker and scrubs the URL
 * up front, so it fires at most once and a failed store cannot replay the JWT
 * from the URL on the next boot. Returns the result on a stored bearer, or
 * `null` when there is nothing to capture.
 */
export async function captureComposioSignin(
  client: OpenCompanyClient,
): Promise<ComposioCaptureResult | null> {
  const jwt = readComposioCallbackJwt();
  if (!jwt) return null;
  const pending = readPending();
  // A sign-in callback with no pending marker is not ours — don't adopt a bearer
  // the operator never asked this console to set.
  if (!pending) return null;
  // Consume BEFORE the network call: the JWT is a credential, so scrub it from
  // the URL and clear the marker immediately, whatever the store's outcome.
  clearPending();
  clearCallbackFromUrl();
  await setComposioToken(client, pending.company, jwt);
  return { company: pending.company };
}
