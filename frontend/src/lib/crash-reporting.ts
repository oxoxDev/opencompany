// The pure half of console crash reporting: the enable/disable decision, the
// secret scrubber, and the event sanitizer.
//
// Kept apart from `@/lib/sentry` — which is the only module that imports the
// SDK — for the reason `src/observability/config.rs` is kept apart from
// `src/observability/mod.rs` on the host: these are the parts that have to be
// provably right, and a module with no runtime import of `@sentry/react` can be
// exercised by the fast `vitest` unit suite (`test/unit/`, `environment: node`)
// rather than only by a browser.
//
// The contract this implements is `docs/spec/runtime/crash-reporting.md`.
// The scrubber is a deliberate port of `src/observability/redaction.rs` — same
// token-wise algorithm, same prefix and key vocabularies — so the doc can
// describe one behaviour rather than two that drift.

import type { ErrorEvent, Event } from "@sentry/react";

/**
 * A transaction event, as `beforeSendTransaction` receives it.
 *
 * Declared here rather than imported. `@sentry/browser`'s `exports.d.ts` — what
 * `@sentry/react` re-exports — publishes `ErrorEvent` but **not**
 * `TransactionEvent`, and reaching past it into `@sentry/core` would be
 * importing from a package this app does not depend on and does not pin. This
 * is the SDK's own definition, which is `Event` narrowed by its discriminant.
 */
export type TransactionEvent = Event & { type: "transaction" };

/** What a redacted span is replaced with, on both surfaces. */
export const REDACTED = "[redacted]";

/** The Sentry surface tag every event from this app carries. */
export const SURFACE = "console";

/** The message body of the one-shot smoke event. See `VITE_SENTRY_SMOKE_TEST`. */
export const SMOKE_TEST_MESSAGE = "console-sentry-smoke-test";

/**
 * Prefixes that identify a credential on their own, whatever surrounds them.
 * Case-sensitive: `AKIA` is an AWS key id and `akia` is a word.
 *
 * Mirrors `SECRET_PREFIXES` in `src/observability/redaction.rs`.
 */
const SECRET_PREFIXES = [
  "sk-",
  "sk_",
  "rk_",
  "pk_",
  "ghp_",
  "gho_",
  "ghs_",
  "ghu_",
  "ghr_",
  "github_pat_",
  "glpat-",
  "xoxb-",
  "xoxp-",
  "xoxa-",
  "xoxs-",
  "xoxe-",
  "xapp-",
  "AKIA",
  "ASIA",
  "th_",
  "shpat_",
  "shpss_",
  "npm_",
  "dop_v1_",
  "SG.",
];

/** The shortest a prefixed token may be before it is treated as a credential. */
const MIN_PREFIXED_LENGTH = 12;

/** Keys whose *value* is a credential, compared after `normalizeKey`. */
const SECRET_KEYS = new Set([
  "token",
  "accesstoken",
  "refreshtoken",
  "idtoken",
  "authtoken",
  "sessiontoken",
  "bearertoken",
  "apikey",
  "apitoken",
  "apisecret",
  "secret",
  "secretkey",
  "clientsecret",
  "password",
  "passwd",
  "pwd",
  "passphrase",
  "authorization",
  "credential",
  "credentials",
  "privatekey",
  "signingkey",
  "dsn",
]);

/**
 * Auth schemes as *values*: exempt, because redacting `Bearer` and leaving the
 * credential standing is worse than doing nothing — the message then looks
 * scrubbed.
 */
const AUTH_SCHEMES = new Set(["bearer", "basic", "digest", "negotiate", "token"]);

/**
 * The subset of `AUTH_SCHEMES` that, as a *key*, licenses redacting the next
 * token across a bare space. `token` is excluded on purpose: as an English word
 * it is far too common ("the token was rejected" would lose `was`), and a real
 * `Authorization: token <pat>` is caught by the prefix rule anyway.
 */
const SCHEME_KEYS = new Set(["bearer", "basic", "digest", "negotiate"]);

/**
 * One token: a maximal run of the characters a credential is written from.
 *
 * `=` and `/` are excluded even though base64 uses both — including `=` would
 * swallow the `token=value` separator this pass depends on, and including `/`
 * would make a whole URL one token. Trailing `==` padding left behind reveals
 * nothing.
 */
const TOKEN_PATTERN = /[A-Za-z0-9_.+~-]+/g;

/** Only the punctuation an assignment is written with may separate a pair. */
const SEPARATOR_PATTERN = /^[ \t:="'>]*$/;

/**
 * Query-parameter names whose value is a credential *in a URL*, on top of
 * everything in `SECRET_KEYS`.
 *
 * Separate from `SECRET_KEYS` because these words are only unambiguous inside a
 * query string. `code` is the magic-link sign-in code this console redeems
 * (`App.tsx`), and a 43-character `?code=` in a breadcrumb or a span is a
 * working sign-in for whoever can read the operator's Sentry project — but
 * `code` also appears in ordinary prose and in `code=ECONNREFUSED`, so
 * redacting it everywhere would eat diagnostics. Inside `?…=` there is no such
 * ambiguity.
 *
 * The gap this closes is real and not hypothetical: `clearMagicLinkFromUrl()`
 * calls `history.replaceState` after the SDK's history instrumentation is
 * installed, so the navigation breadcrumb records the URL *before* the code was
 * removed. Performance tracing adds a second path to the same string, since a
 * navigation span carries the URL too.
 */
const SECRET_QUERY_KEYS = new Set(["code", "state", "sig", "signature"]);

/**
 * Redacts the values of credential-bearing query parameters, wherever a URL
 * appears in `text`.
 *
 * Runs before the token pass, which cannot see these: `?code=abc` has no
 * separator the token pass treats as an assignment for a key it knows.
 */
/**
 * Whether a query parameter's name means its value is a credential.
 *
 * `decodeURIComponent` **throws** `URIError` on a malformed escape — a URL as
 * ordinary as `https://host/?%E0%A4=value` is enough. Uncaught, that threw out
 * of `scrubSecrets`, out of `beforeSend`, and cost the **entire report**: one
 * malformed URL anywhere in an event and nothing was sent at all, silently.
 *
 * So the failure is caught, and it fails **closed**: a key that cannot be
 * decoded is treated as naming a credential. A name that malformed is not one
 * this app produces, and over-redacting an unreadable parameter costs a
 * diagnostic, where under-redacting it costs a credential.
 */
function queryKeyNamesASecret(key: string): boolean {
  let decoded: string;
  try {
    decoded = decodeURIComponent(key);
  } catch {
    return true;
  }
  const normalized = normalizeKey(decoded);
  return SECRET_QUERY_KEYS.has(normalized) || SECRET_KEYS.has(normalized);
}

function scrubUrlQuery(text: string): string {
  // A URL's query runs from `?` to whitespace or one of the characters that
  // ends a URL in prose — the same set `scrubUrlUserinfo` stops at, minus `?`.
  return text.replace(/\?[^\s"'<>),;]*/g, (query) => {
    if (!query.includes("=")) return query;
    return query.replace(/([?&])([^=&]+)=([^&]*)/g, (pair, lead, key, value) => {
      const secret = queryKeyNamesASecret(key);
      return secret && value.length > 0 ? `${lead}${key}=${REDACTED}` : pair;
    });
  });
}

/**
 * The comparable form of a key token: everything after the last `.`, with `-`
 * and `_` removed, lower-cased.
 *
 * This is what makes `cancellation_token` safe without a word-boundary hack:
 * it is one token and normalizes to `cancellationtoken`, which is not a key.
 */
function normalizeKey(token: string): string {
  return token
    .slice(token.lastIndexOf(".") + 1)
    .replace(/[-_]/g, "")
    .toLowerCase();
}

/** Whether a token names itself as a credential. */
function looksLikeASecret(token: string): boolean {
  if (
    token.length >= MIN_PREFIXED_LENGTH &&
    SECRET_PREFIXES.some((prefix) => token.startsWith(prefix))
  ) {
    return true;
  }
  // A JWT: three base64url segments whose header always begins `eyJ`. Session
  // headers on this surface are JWTs and carry no issuer prefix.
  return token.length >= 20 && token.startsWith("eyJ") && token.split(".").length >= 3;
}

/** Whether `key`, followed by `separator`, means the next token is its value. */
function keyDirectsASecret(key: string, separator: string): boolean {
  // A separator that crosses a line is not an assignment; it is two unrelated
  // log lines that happen to be adjacent.
  if (/[\n\r]/.test(separator) || !SEPARATOR_PATTERN.test(separator)) return false;
  const normalized = normalizeKey(key);
  if (SCHEME_KEYS.has(normalized)) return true;
  if (!SECRET_KEYS.has(normalized)) return false;
  return separator.includes("=") || separator.includes(":") || key.startsWith("-");
}

/** The token pass: split into words, then ask whole-word questions. */
function scrubTokens(text: string): string {
  let out = "";
  let copied = 0;
  let changed = false;
  let previous: string | null = null;
  let separatorStart = 0;

  for (const match of text.matchAll(TOKEN_PATTERN)) {
    const start = match.index ?? 0;
    const token = match[0];
    const end = start + token.length;
    const directed =
      previous !== null &&
      keyDirectsASecret(previous, text.slice(separatorStart, start)) &&
      !AUTH_SCHEMES.has(normalizeKey(token));

    if (looksLikeASecret(token) || directed) {
      out += text.slice(copied, start) + REDACTED;
      copied = end;
      changed = true;
    }
    previous = token;
    separatorStart = end;
  }

  return changed ? out + text.slice(copied) : text;
}

/**
 * The URL pass: `https://user:pass@host/path` loses its userinfo.
 *
 * Separate from the token pass because `:` and `@` are exactly the characters
 * that pass uses as separators, so a URL's credential is invisible to it.
 */
function scrubUrlUserinfo(text: string): string {
  let out = "";
  let copied = 0;
  let changed = false;
  let index = 0;

  for (;;) {
    const offset = text.indexOf("://", index);
    if (offset < 0) break;
    const authorityStart = offset + 3;
    const delimiter = text.slice(authorityStart).search(/[/?#"'<>),;\s]/);
    const authorityEnd = delimiter < 0 ? text.length : authorityStart + delimiter;
    // `lastIndexOf`, not `indexOf`: a password may itself contain an `@`, and
    // the last one is the delimiter the URL grammar means.
    const at = text.slice(authorityStart, authorityEnd).lastIndexOf("@");
    if (at >= 0) {
      out += text.slice(copied, authorityStart) + REDACTED;
      // The `@` stays, so the result still reads as a URL.
      copied = authorityStart + at;
      changed = true;
    }
    index = authorityEnd;
    if (index >= text.length) break;
  }

  return changed ? out + text.slice(copied) : text;
}

/**
 * Removes credentials from `text`.
 *
 * A last line of defence, not the first. The first is not putting a credential
 * in a message at all — a scrubber is heuristic by construction and cannot
 * recognise a secret that looks like a word.
 */
export function scrubSecrets(text: string): string {
  return scrubTokens(scrubUrlQuery(scrubUrlUserinfo(text)));
}

/**
 * What leaves the browser, and what does not. Wired as `beforeSend`.
 *
 * Mutates and returns the event rather than rebuilding it, because the SDK
 * hands over ownership and a rebuild would silently drop any field added by a
 * future SDK version — the opposite of the safe direction.
 *
 * Never returns `null`: this is a scrubber, not a filter. Deciding which errors
 * are worth seeing belongs in the operator's own Sentry project, where they can
 * see what they are suppressing.
 */
/**
 * Scrubs an arbitrary structure in place: every string leaf goes through
 * [`scrubSecrets`], and every value under a key that *names* a credential is
 * replaced outright, whatever its type.
 *
 * Two rules, because one is not enough:
 *
 * * **String leaves**, at any depth. The `trace` context's `data` holds
 *   `url.full` — the address bar, verbatim, magic-link `?code=` included —
 *   which is not a hypothetical: it was found in a captured outbound envelope
 *   while the span descriptions beside it were correctly redacted.
 * * **Keys**, because `scrubSecrets` reads text and `{ token: "hunter2" }` has
 *   no text to read. The string rule sees `hunter2`, a word with no prefix and
 *   no `key=` beside it, and leaves it alone. Structure carries the label that
 *   the flat string form would have carried inline, so the structure has to be
 *   read as well. A matching key takes the whole value — an object under
 *   `credentials` is not made safe by scrubbing its leaves one at a time.
 *
 * Recursion is the point. The version this replaces looked at `context.data`
 * and only at its top level, so `{ token: "…" }` directly on a context and
 * `data: { request: { authorization: "…" } }` both went out untouched.
 */
function scrubDeep(value: unknown, key?: string): unknown {
  if (key !== undefined && SECRET_KEYS.has(normalizeKey(key))) return REDACTED;
  if (typeof value === "string") return scrubSecrets(value);
  if (Array.isArray(value)) {
    for (let i = 0; i < value.length; i += 1) value[i] = scrubDeep(value[i]);
    return value;
  }
  if (value && typeof value === "object") {
    const record = value as Record<string, unknown>;
    for (const [name, child] of Object.entries(record)) {
      record[name] = scrubDeep(child, name);
    }
    return record;
  }
  // Numbers, booleans and null carry no credential and no free-form text.
  return value;
}

/**
 * Every free-form surface an event of **either** kind can carry, scrubbed in
 * one place.
 *
 * # Why this is shared rather than repeated
 *
 * The previous version was two hand-maintained lists, one in each sanitizer,
 * and a field was covered exactly when someone remembered to add it to both.
 * `tags` is what that cost: `{ password: "hunter2" }` went out in clear from
 * both hooks, because the tag loop scrubbed values as *text* and `hunter2` has
 * no credential prefix and no `key=` beside it — the same key-shaped hole that
 * was closed for contexts, spans, breadcrumbs and `extra` one round earlier,
 * on a surface the sweep did not reach.
 *
 * So the enumeration lives here once, taken from `@sentry/core`'s `Event`
 * rather than from memory, and each sanitizer adds only the policy that is
 * genuinely different between them (which contexts survive, what happens to
 * `request`).
 *
 * Deliberately NOT scrubbed, each for a reason:
 *
 * * `modules` — a map of package name to version, produced by the SDK.
 * * `measurements` — numbers.
 * * `release`, `environment`, `dist`, `platform` — set by `Sentry.init` from
 *   this build's own configuration.
 * * `event_id`, timestamps, `level` — not text.
 */
function scrubFreeFormFields(event: Event): void {
  if (event.message) event.message = scrubSecrets(event.message);
  // The route or operation name. Safe when the SDK derived it, but an app can
  // set it to anything.
  if (event.transaction) event.transaction = scrubSecrets(event.transaction);
  if (event.logger) event.logger = scrubSecrets(event.logger);
  if (event.logentry) {
    if (event.logentry.message) event.logentry.message = scrubSecrets(event.logentry.message);
    if (event.logentry.params) scrubDeep(event.logentry.params);
  }
  // Grouping keys, which an app builds from whatever distinguishes the error.
  if (event.fingerprint) event.fingerprint = event.fingerprint.map(scrubSecrets);
  // The SDK's own scratch bag. It is not meant to be serialized, but it is on
  // the event this hook is handed, and it can hold anything.
  delete event.sdkProcessingMetadata;

  for (const exception of event.exception?.values ?? []) {
    if (exception.value) exception.value = scrubSecrets(exception.value);
    if (exception.mechanism) delete exception.mechanism.data;
    // Frame locals and source snippets: a captured local is a captured
    // credential, and source context is this repository's own code, which the
    // operator already has.
    for (const frame of exception.stacktrace?.frames ?? []) {
      delete frame.vars;
      delete frame.context_line;
      delete frame.pre_context;
      delete frame.post_context;
    }
  }

  // Breadcrumbs are kept, unlike the vendored runtime's console, which drops
  // them wholesale. They are the single most useful thing in a browser crash
  // report — which route, which request, which status — and the integrations
  // that carry *content* (`console`, `dom`) are switched off at the source in
  // `@/lib/sentry` instead. What is left is a method, a URL and a status.
  for (const breadcrumb of event.breadcrumbs ?? []) {
    if (breadcrumb.message) breadcrumb.message = scrubSecrets(breadcrumb.message);
    if (breadcrumb.data) scrubDeep(breadcrumb.data);
  }

  for (const span of event.spans ?? []) {
    if (span.description) span.description = scrubSecrets(span.description);
    if (span.data) scrubDeep(span.data);
    // A span carries a tag map of its own, with the same key-shaped hole the
    // event's has. Reached through a cast because `SpanJSON` does not declare
    // it — which is exactly why it was missed, and why the value is checked
    // at run time rather than trusted to the type.
    const tags = (span as { tags?: unknown }).tags;
    if (tags && typeof tags === "object") scrubDeep(tags);
  }

  // `scrubDeep`, not a value-only loop: a tag KEY is as much a label as a
  // nested one, and this is the surface that proved it.
  if (event.tags) scrubDeep(event.tags);
}

export function sanitizeEvent(event: ErrorEvent): ErrorEvent {
  // Identity. `sendDefaultPii: false` already withholds the IP address; this
  // covers the fields an integration could populate later. `user` is never set
  // by this app: the only stable per-person identifier the console holds is an
  // email address, and that is exactly what must not leave.
  event.server_name = undefined;
  event.user = undefined;

  // The request envelope, narrowed to the one header that earns its place.
  // Sentry derives `os` / `browser` / `device` server-side by parsing the
  // User-Agent, so dropping the envelope outright loses all platform context
  // (the lesson `httpContextIntegration` is re-added for). The URL, its query
  // string — where a magic-link `?code=` lives — and any cookie go.
  const userAgent = (event.request?.headers as Record<string, string> | undefined)?.[
    "User-Agent"
  ];
  event.request = userAgent ? { headers: { "User-Agent": userAgent } } : undefined;

  // `extra` is the free-form bag anything can be attached to, and nothing in
  // this app attaches to it deliberately.
  delete event.extra;

  // Contexts, allow-listed. Anything the SDK did not derive from the platform
  // is dropped rather than scrubbed, because an unknown context has unknown
  // shape and an allow-list fails in the safe direction.
  //
  // `trace` is on the list and must stay on it: it is what associates this
  // error with the transaction it happened inside, so dropping it would break
  // exactly the "which request caused this" link tracing exists for. It is the
  // one allow-listed context that carries free-form `data` — `url.full` among
  // it — so unlike the platform three it is scrubbed rather than trusted.
  event.contexts = {
    os: event.contexts?.os,
    browser: event.contexts?.browser,
    device: event.contexts?.device,
    trace: event.contexts?.trace && (scrubDeep(event.contexts.trace) as typeof event.contexts.trace),
  };

  scrubFreeFormFields(event);
  event.tags = { ...event.tags, surface: SURFACE };

  return event;
}

// ---------------------------------------------------------------------------
// The enable/disable decision
// ---------------------------------------------------------------------------

/**
 * The TinyHumans console (`opencompany-frontend`) project's DSN, used by a
 * production bundle built without `VITE_SENTRY_DSN`. A browser DSN is public by
 * construction; it authorizes writes to this one project only. Build with
 * `VITE_SENTRY_DSN=off` for a bundle that reports nothing.
 */
export const DEFAULT_CONSOLE_SENTRY_DSN =
  "https://ee905504c09005339f499082282ce189@sentry.tinyhumans.ai/13";

/** The build-time variables this surface reads. */
export interface CrashReportingEnv {
  /**
   * The DSN. Unset or blank means {@link DEFAULT_CONSOLE_SENTRY_DSN} in a
   * production build and silence under the Vite dev server; `off` means
   * silence; an unusable value means silence.
   */
  VITE_SENTRY_DSN?: string;
  /** Overrides the `environment` tag. */
  VITE_SENTRY_ENVIRONMENT?: string;
  /** `true` fires one smoke event at init. Anything else does nothing. */
  VITE_SENTRY_SMOKE_TEST?: string;
  /**
   * The fraction of page loads traced, `0` to `1`. **Absent means `0`**, and
   * that is the point: transactions are billed separately from errors and are
   * emitted whether or not anything went wrong, so a rate this repository chose
   * would be a recurring bill nobody asked for. Mirrors the host's
   * `OPENCOMPANY_SENTRY_TRACES_SAMPLE_RATE`.
   */
  VITE_SENTRY_TRACES_SAMPLE_RATE?: string;
  /**
   * Comma-separated origins the browser may attach `sentry-trace` and
   * `baggage` headers to, on top of same-origin requests.
   *
   * Needed only when the console talks to a host on another origin — a Vite dev
   * server proxies `/api`, and a bundle served by the host is same-origin, so
   * both are covered by the default. Set it to the host's origin
   * (`https://api.example.com`) and a console action and the request it caused
   * become one trace instead of two.
   */
  VITE_SENTRY_TRACE_PROPAGATION_TARGETS?: string;
  /** Vite's own dev-server flag, which names the default environment. */
  DEV?: boolean;
}

/** Everything `Sentry.init` needs, once the decision is to report. */
export interface CrashReportingConfig {
  dsn: string;
  environment: string;
  release: string;
  smokeTest: boolean;
  /** `0` — the default — means no `browserTracingIntegration` is installed. */
  tracesSampleRate: number;
  /**
   * `true` when `VITE_SENTRY_TRACES_SAMPLE_RATE` was set to something that is
   * not a fraction between 0 and 1. Distinct from "not set" so the caller can
   * say so once, rather than leaving an operator who typed `50%` unable to tell
   * their typo from a working default. The rate is `0` either way.
   */
  tracesUnreadable: boolean;
  /** What `sentry-trace` / `baggage` headers may be attached to. */
  tracePropagationTargets: (string | RegExp)[];
}

/**
 * Same-origin requests, and nothing else.
 *
 * The default because it is the only one that cannot leak: a `sentry-trace`
 * header sent to a third party tells them this app is instrumented and hands
 * them a trace id that correlates their logs with the operator's. Relative URLs
 * cover both shapes the console actually runs in — the Vite dev server proxies
 * `/api` to the host, and a bundle the host serves is same-origin with it.
 */
const SAME_ORIGIN_ONLY: RegExp[] = [/^\//];

/**
 * The traced fraction, and whether the value was readable.
 *
 * `100` is refused rather than clamped to `1`, for the reason the host's
 * `config::traces` gives: it far more likely means "100%" than "1.0", and
 * clamping would trace every page load for someone who meant nothing of the
 * kind.
 */
function resolveTracesSampleRate(raw: string | undefined): {
  rate: number;
  unreadable: boolean;
} {
  const trimmed = (raw ?? "").trim();
  if (!trimmed) return { rate: 0, unreadable: false };
  const rate = Number(trimmed);
  if (!Number.isFinite(rate) || rate < 0 || rate > 1) {
    return { rate: 0, unreadable: true };
  }
  return { rate, unreadable: false };
}

/** The origins trace headers may be attached to, on top of same-origin. */
function resolveTracePropagationTargets(raw: string | undefined): (string | RegExp)[] {
  const extra = (raw ?? "")
    .split(",")
    .map((target) => target.trim())
    .filter((target) => target.length > 0);
  return [...SAME_ORIGIN_ONLY, ...extra];
}

/**
 * Whether a string is a Sentry DSN.
 *
 * The same four checks `observability::config::parse_dsn` makes on the host,
 * through the platform's own URL parser rather than a second hand-rolled
 * reader: an `http`/`https` scheme, a public key, a host, and a project id. A
 * URL that parses but is not a DSN is precisely the input that resolves to
 * "reporting" and then never delivers.
 *
 * A DSN carrying a password is refused rather than repaired: the
 * `https://key:secret@…` form has not been accepted by any ingest since 2016,
 * so it is either a stale copy or a credential pasted into the wrong variable.
 */
function isUsableDsn(candidate: string): boolean {
  let url: URL;
  try {
    url = new URL(candidate);
  } catch {
    return false;
  }
  if (url.protocol !== "https:" && url.protocol !== "http:") return false;
  if (!url.username || url.password) return false;
  if (!url.hostname) return false;
  return url.pathname.split("/").pop() !== "";
}

/**
 * Resolves what this bundle will do, or `null` for silence.
 *
 * `release` is passed in rather than computed here: it is built once in
 * `vite.config.ts`, where the source-map upload needs the same string, and a
 * release the bundle and the upload disagree about is un-symbolicated stack
 * traces with no error to explain them.
 */
export function resolveCrashReporting(
  env: CrashReportingEnv,
  release: string,
): CrashReportingConfig | null {
  const configured = (env.VITE_SENTRY_DSN ?? "").trim();
  if (configured.toLowerCase() === "off") return null;
  // A dev server never falls back: a contributor's `npm run dev` must not
  // write to the production project on a default nobody chose.
  const dsn = configured || (env.DEV ? "" : DEFAULT_CONSOLE_SENTRY_DSN);
  if (!dsn || !isUsableDsn(dsn)) return null;
  const environment = (env.VITE_SENTRY_ENVIRONMENT ?? "").trim().toLowerCase();
  const traces = resolveTracesSampleRate(env.VITE_SENTRY_TRACES_SAMPLE_RATE);
  return {
    dsn,
    // The host defaults this to its deployment kind (`self-hosted`,
    // `hosted-tenant`, `desktop`), which a browser bundle cannot know. Set
    // `VITE_SENTRY_ENVIRONMENT` to the host's value when the two surfaces
    // should line up in one Sentry filter.
    environment: environment || (env.DEV ? "development" : "production"),
    release,
    // Exactly `true`. Vite env values are always strings, so a loose check
    // would make `VITE_SENTRY_SMOKE_TEST=false` fire the event.
    smokeTest: env.VITE_SENTRY_SMOKE_TEST === "true",
    tracesSampleRate: traces.rate,
    tracesUnreadable: traces.unreadable,
    tracePropagationTargets: resolveTracePropagationTargets(
      env.VITE_SENTRY_TRACE_PROPAGATION_TARGETS,
    ),
  };
}

/**
 * What leaves the browser for a **transaction**, and what does not.
 *
 * A separate hook because `beforeSend` is never called for one: the SDK routes
 * transactions through `beforeSendTransaction`, so everything `sanitizeEvent`
 * guarantees would simply not apply to the larger, more frequent payload. The
 * host has the same split and a worse version of the problem — sentry 0.47 has
 * no transaction hook at all, so it scrubs at the transport instead.
 *
 * Spans are where the content is. A `browserTracing` page load carries one span
 * per `fetch`/`xhr`, each with the request URL — including the magic-link
 * `?code=` a navigation can still hold.
 */
export function sanitizeTransaction(event: TransactionEvent): TransactionEvent | null {
  event.server_name = undefined;
  event.user = undefined;
  event.request = undefined;
  delete event.extra;

  // Not allow-listed the way an error event's contexts are: a transaction's
  // `trace` context IS the transaction, so keeping three platform entries and
  // dropping the rest would throw the payload away. Scrubbed instead — and
  // `contexts.trace.data["url.full"]` is why, since it is the address bar
  // verbatim, magic-link code and all.
  for (const context of Object.values(event.contexts ?? {})) {
    if (context && typeof context === "object") scrubDeep(context);
  }

  // Everything else — name, spans, breadcrumbs, tags, fingerprint — is the
  // same enumeration an error event gets, and is shared with it so the two
  // cannot drift apart again.
  scrubFreeFormFields(event);
  event.tags = { ...event.tags, surface: SURFACE };

  return event;
}
