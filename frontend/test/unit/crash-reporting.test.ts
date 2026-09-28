// The console half of `docs/spec/runtime/crash-reporting.md`: the decision to
// report at all, and what a report is allowed to carry.
//
// These are the two things that have to be right, and both are pure, so they
// are tested here rather than only in a browser. `@/lib/crash-reporting`
// imports no Sentry runtime for exactly this reason — its only `@sentry/react`
// import is a type, which is erased.

import { describe, expect, it } from "vitest";

import {
  DEFAULT_CONSOLE_SENTRY_DSN,
  REDACTED,
  resolveCrashReporting,
  sanitizeEvent,
  sanitizeTransaction,
  scrubSecrets,
} from "@/lib/crash-reporting";
import type { TransactionEvent } from "@/lib/crash-reporting";
import type { ErrorEvent } from "@sentry/react";

const DSN = "https://examplePublicKey@o0.ingest.sentry.io/0";
const RELEASE = "opencompany@0.1.0+d31e532f7c8a";

/**
 * A credential-shaped string, assembled rather than written down.
 *
 * `looksLikeASecret` reads a token's prefix and its length and nothing else, so
 * the high-entropy body a real credential carries is filler as far as these
 * tests are concerned — what `scrubSecrets` is handed is identical either way.
 *
 * Written out as a literal, though, `ghp_AAAA…` is byte-for-byte what a leaked
 * token looks like to everything that reads this repository: secret scanners
 * flag the file on every push, and after a genuine incident somebody grepping
 * the tree has to rule each fixture out by hand before they can believe it is
 * clean. A scanner that is permanently red about a test fixture is a scanner
 * nobody reads.
 *
 * So the prefix — the part under test, and the part that has to stay readable —
 * is written down, and only the body is assembled here. Mirrors
 * `credential_shaped` in `src/observability/redaction.rs`.
 */
function credentialShaped(prefix: string, bodyLength: number, fill = "A"): string {
  return prefix + fill.repeat(bodyLength);
}

describe("resolveCrashReporting", () => {
  it("falls back to the compiled-in console DSN in a production build", () => {
    for (const env of [{}, { VITE_SENTRY_DSN: "" }, { VITE_SENTRY_DSN: "   " }]) {
      expect(resolveCrashReporting(env, RELEASE)?.dsn).toBe(DEFAULT_CONSOLE_SENTRY_DSN);
    }
  });

  it("reports nothing without a DSN under the dev server, or when switched off", () => {
    expect(resolveCrashReporting({ DEV: true }, RELEASE)).toBeNull();
    expect(resolveCrashReporting({ VITE_SENTRY_DSN: "", DEV: true }, RELEASE)).toBeNull();
    expect(resolveCrashReporting({ VITE_SENTRY_DSN: "off" }, RELEASE)).toBeNull();
    expect(resolveCrashReporting({ VITE_SENTRY_DSN: " OFF " }, RELEASE)).toBeNull();
  });

  it("reports when a DSN is configured", () => {
    expect(resolveCrashReporting({ VITE_SENTRY_DSN: DSN }, RELEASE)).toEqual({
      dsn: DSN,
      environment: "production",
      release: RELEASE,
      smokeTest: false,
      // Agreeing to crash reports is not agreeing to a per-page-load
      // transaction feed, which is billed separately.
      tracesSampleRate: 0,
      tracesUnreadable: false,
      tracePropagationTargets: [/^\//],
    });
  });

  it("traces nothing until a rate is asked for", () => {
    for (const value of [undefined, "", "  ", "0", "0.0"]) {
      const config = resolveCrashReporting(
        { VITE_SENTRY_DSN: DSN, VITE_SENTRY_TRACES_SAMPLE_RATE: value },
        RELEASE,
      );
      expect(config?.tracesSampleRate).toBe(0);
      expect(config?.tracesUnreadable).toBe(false);
    }
  });

  it("takes a rate between zero and one as asked", () => {
    for (const [raw, expected] of [
      ["1", 1],
      ["0.1", 0.1],
      [" 0.25 ", 0.25],
    ] as const) {
      const config = resolveCrashReporting(
        { VITE_SENTRY_DSN: DSN, VITE_SENTRY_TRACES_SAMPLE_RATE: raw },
        RELEASE,
      );
      expect(config?.tracesSampleRate).toBe(expected);
      expect(config?.tracesUnreadable).toBe(false);
    }
  });

  it("refuses a rate that is not a fraction rather than clamping it", () => {
    // `100` almost certainly means "100%". Clamping it to 1 would trace every
    // page load for someone who meant nothing of the kind.
    for (const raw of ["100", "50%", "-1", "1.5", "abc", "0,5", "NaN", "Infinity"]) {
      const config = resolveCrashReporting(
        { VITE_SENTRY_DSN: DSN, VITE_SENTRY_TRACES_SAMPLE_RATE: raw },
        RELEASE,
      );
      expect(config?.tracesSampleRate, raw).toBe(0);
      expect(config?.tracesUnreadable, raw).toBe(true);
    }
  });

  it("propagates trace headers same-origin only until a host is named", () => {
    expect(resolveCrashReporting({ VITE_SENTRY_DSN: DSN }, RELEASE)?.tracePropagationTargets).toEqual(
      [/^\//],
    );
    expect(
      resolveCrashReporting(
        {
          VITE_SENTRY_DSN: DSN,
          VITE_SENTRY_TRACE_PROPAGATION_TARGETS: " https://api.example.com , https://b.example ",
        },
        RELEASE,
      )?.tracePropagationTargets,
    ).toEqual([/^\//, "https://api.example.com", "https://b.example"]);
  });

  it("refuses a string that is not a Sentry DSN", () => {
    // Each of these nearly parses, and each would resolve to a client that
    // never delivers — which is the failure the check exists to prevent.
    for (const candidate of [
      "o0.ingest.sentry.io/0", // no scheme
      "ftp://key@o0.ingest.sentry.io/0", // a scheme nothing can POST to
      "https://o0.ingest.sentry.io/0", // no public key
      "https://key@o0.ingest.sentry.io/", // no project id
      "https://key:secret@o0.ingest.sentry.io/0", // the pre-2016 secret form
      "not a dsn",
    ]) {
      expect(resolveCrashReporting({ VITE_SENTRY_DSN: candidate }, RELEASE)).toBeNull();
    }
  });

  it("names the environment, defaulting by build kind", () => {
    expect(resolveCrashReporting({ VITE_SENTRY_DSN: DSN, DEV: true }, RELEASE)?.environment).toBe(
      "development",
    );
    expect(
      resolveCrashReporting(
        { VITE_SENTRY_DSN: DSN, VITE_SENTRY_ENVIRONMENT: "  Hosted-Tenant " },
        RELEASE,
      )?.environment,
    ).toBe("hosted-tenant");
  });

  it("arms the smoke test only on an exact `true`", () => {
    // Vite env values are always strings, so a loose check would make
    // `VITE_SENTRY_SMOKE_TEST=false` fire the event.
    expect(
      resolveCrashReporting({ VITE_SENTRY_DSN: DSN, VITE_SENTRY_SMOKE_TEST: "true" }, RELEASE)
        ?.smokeTest,
    ).toBe(true);
    for (const value of ["false", "1", "TRUE", "yes", ""]) {
      expect(
        resolveCrashReporting({ VITE_SENTRY_DSN: DSN, VITE_SENTRY_SMOKE_TEST: value }, RELEASE)
          ?.smokeTest,
      ).toBe(false);
    }
  });
});

describe("scrubSecrets", () => {
  it("removes a labelled credential and keeps the diagnostic", () => {
    for (const [input, expected] of [
      ["api_key=hunter2", `api_key=${REDACTED}`],
      ["api-key: hunter2", `api-key: ${REDACTED}`],
      [`"token":"hunter2"`, `"token":"${REDACTED}"`],
      ["password = hunter2", `password = ${REDACTED}`],
      ["--token hunter2", `--token ${REDACTED}`],
      ["Authorization: Bearer hunter2", `Authorization: Bearer ${REDACTED}`],
    ]) {
      expect(scrubSecrets(input)).toBe(expected);
    }
  });

  it("never mistakes the auth scheme for the credential", () => {
    // Redacting `Bearer` and leaving the credential is worse than doing
    // nothing: the message then looks scrubbed.
    const scrubbed = scrubSecrets("Authorization: Bearer hunter2");
    expect(scrubbed).toContain("Bearer");
    expect(scrubbed).not.toContain("hunter2");
  });

  it("removes a credential that identifies itself", () => {
    // One per issuer prefix the host's `SECRET_PREFIXES` names, plus the JWT
    // arm — the two lists are a deliberate port of each other, so they are
    // covered alike. Assembled by `credentialShaped`, which says why.
    for (const secret of [
      credentialShaped("sk-ant-api03-", 28),
      credentialShaped("sk-proj-", 20),
      credentialShaped("sk_live_", 20),
      credentialShaped("ghp_", 36),
      `${credentialShaped("github_pat_", 20)}_${credentialShaped("", 8, "B")}`,
      credentialShaped("glpat-", 20),
      `xoxb-${credentialShaped("", 10, "1")}-${credentialShaped("", 10, "2")}-${credentialShaped("", 12)}`,
      credentialShaped("AKIA", 16),
      credentialShaped("th_live_", 20),
      credentialShaped("npm_", 28),
      `${credentialShaped("SG.", 22)}.${credentialShaped("", 12, "B")}`,
      // A JWT: `eyJ` and two dots are the whole of the rule.
      `${credentialShaped("eyJ", 17)}.${credentialShaped("", 16, "B")}.c2ln`,
    ]) {
      const scrubbed = scrubSecrets(`the host said: ${secret} was rejected`);
      expect(scrubbed).not.toContain(secret);
      expect(scrubbed).toContain("was rejected");
    }
  });

  it("strips userinfo from a URL", () => {
    expect(scrubSecrets("GET https://user:hunter2@host.example/api/v1 failed")).toBe(
      `GET https://${REDACTED}@host.example/api/v1 failed`,
    );
    // A port colon is not an assignment.
    expect(scrubSecrets("GET https://host.example:8080/api/v1")).toBe(
      "GET https://host.example:8080/api/v1",
    );
  });

  it("never throws on a malformed percent-escape", () => {
    // `decodeURIComponent` throws `URIError` on these. Uncaught it came out of
    // `scrubSecrets`, out of `beforeSend`, and cost the WHOLE report — one
    // malformed URL and nothing was sent at all, silently.
    for (const input of [
      "GET https://host/?%E0%A4=value failed",
      "https://host/?%=1",
      "https://host/?%zz=secret&page=2",
      "https://host/?a=1&%E0%A4=2",
    ]) {
      expect(() => scrubSecrets(input)).not.toThrow();
    }
    // And it fails CLOSED: a key too malformed to decode is treated as naming
    // a credential, because over-redacting an unreadable parameter costs a
    // diagnostic while under-redacting it costs a credential.
    expect(scrubSecrets("https://host/?%E0%A4=hunter2")).toBe(
      `https://host/?%E0%A4=${REDACTED}`,
    );
    // A readable parameter beside it is still judged on its own merits.
    expect(scrubSecrets("https://host/?%E0%A4=x&page=2")).toContain("page=2");
  });

  it("leaves prose and ordinary diagnostics alone", () => {
    // The word-boundary false positive that plagued the regex version cannot
    // arise: `cancellation_token` is one token and normalizes to something
    // that is not a key.
    for (const input of [
      "the token was rejected by the provider",
      "cancellation_token=abc123",
      "next_page_token=abc123",
      "idempotency_key=abc123",
      "GET /api/v1/companies/acme/agents -> 500 in 42ms",
      "no credential is configured for this company",
    ]) {
      expect(scrubSecrets(input)).toBe(input);
    }
  });
});

describe("sanitizeEvent", () => {
  /** One event carrying something it must not send in every field that can. */
  function hostileEvent(): ErrorEvent {
    return {
      type: undefined,
      message: "refresh failed: api_key=hunter2",
      server_name: "operator-laptop",
      user: { email: "operator@example.com", ip_address: "203.0.113.4" },
      request: {
        url: "https://console.example/#/settings?code=magic-link-code",
        headers: { "User-Agent": "Mozilla/5.0", Cookie: "oc_session=hunter2" },
      },
      extra: { state: "password=hunter2" },
      contexts: {
        os: { name: "macOS" },
        browser: { name: "Chrome" },
        device: { family: "Mac" },
        // Anything could be in here, so nothing is kept.
        redux: { store: "hunter2" },
      },
      tags: { origin: credentialShaped("th_live_", 20) },
      breadcrumbs: [
        {
          message: "GET https://u:hunter2@host.example/api/v1",
          data: { url: "https://host.example/api/v1?token=hunter2", status_code: 500 },
        },
      ],
      exception: {
        values: [
          {
            type: "ApiError",
            value: `token: ${credentialShaped("ghp_", 36)} rejected`,
            mechanism: { type: "generic", data: { secret: "hunter2" } },
            stacktrace: {
              frames: [
                {
                  filename: "app.js",
                  context_line: 'const key = "hunter2";',
                  pre_context: ["function connect() {"],
                  post_context: ["}"],
                  vars: { key: "hunter2" },
                },
              ],
            },
          },
        ],
      },
    };
  }

  it("lets no credential or identity through", () => {
    const rendered = JSON.stringify(sanitizeEvent(hostileEvent()));
    for (const leaked of [
      "hunter2",
      "ghp_AAAA",
      "th_live_AAAA",
      "operator-laptop",
      "operator@example.com",
      "203.0.113.4",
      "magic-link-code",
    ]) {
      expect(rendered).not.toContain(leaked);
    }
  });

  it("keeps what makes a report worth reading", () => {
    const sanitized = sanitizeEvent(hostileEvent());
    // The diagnostic around each redaction.
    expect(sanitized.message).toContain("refresh failed");
    expect(sanitized.exception?.values?.[0]?.value).toContain("rejected");
    // Which request, and what it answered.
    expect(sanitized.breadcrumbs?.[0]?.data?.status_code).toBe(500);
    expect(sanitized.breadcrumbs?.[0]?.data?.url).toContain("host.example");
    // The one header Sentry needs to derive OS / browser / device server-side.
    expect(sanitized.request).toEqual({ headers: { "User-Agent": "Mozilla/5.0" } });
    expect(sanitized.contexts?.browser?.name).toBe("Chrome");
    // And the surface, so console events filter cleanly beside the host's.
    expect(sanitized.tags?.surface).toBe("console");
  });

  it("allow-lists contexts rather than scrubbing them", () => {
    // An unknown context has unknown shape, so the list fails in the safe
    // direction. `redux` above must not survive.
    const sanitized = sanitizeEvent(hostileEvent());
    expect(Object.keys(sanitized.contexts ?? {})).not.toContain("redux");
    expect(sanitized.contexts?.browser?.name).toBe("Chrome");
  });

  it("keeps the trace context, scrubbed, so the error stays on its trace", () => {
    // Dropping `trace` would sever the link between an error and the request
    // that caused it — the whole point of distributed tracing. But its `data`
    // carries `url.full`, which is the address bar verbatim.
    const sanitized = sanitizeEvent({
      type: undefined,
      contexts: {
        trace: {
          trace_id: "ed12b4924c1b4fc3a1b87ba462a40b7c",
          span_id: "8e41772640821824",
          data: {
            "url.full": "http://host/?code=Xj7wQ2mNp4Lk9RtVb3Zc8Hy1Ds5Fg6Ae0Ui2Oq7Pw3",
            "url.path": "/",
          },
        },
      },
    } as unknown as ErrorEvent);
    expect(sanitized.contexts?.trace?.trace_id).toBe("ed12b4924c1b4fc3a1b87ba462a40b7c");
    const rendered = JSON.stringify(sanitized);
    expect(rendered).not.toContain("Xj7wQ2mNp4Lk9RtVb3Zc8Hy1Ds5Fg6Ae0Ui2Oq7Pw3");
    expect(rendered).toContain("url.path");
  });

  it("strips frame locals and source context", () => {
    const frame = sanitizeEvent(hostileEvent()).exception?.values?.[0]?.stacktrace?.frames?.[0];
    expect(frame?.vars).toBeUndefined();
    expect(frame?.context_line).toBeUndefined();
    expect(frame?.pre_context).toBeUndefined();
    expect(frame?.post_context).toBeUndefined();
    // The frame itself survives, or there is no stack trace to read.
    expect(frame?.filename).toBe("app.js");
  });

  it("finds a credential nested in an error event's structured data", () => {
    const rendered = JSON.stringify(
      sanitizeEvent({
        type: undefined,
        breadcrumbs: [
          {
            data: {
              token: "hunter2",
              request: { headers: { authorization: "Bearer hunter2" } },
              status_code: 500,
            },
          },
        ],
      } as unknown as ErrorEvent),
    );
    expect(rendered).not.toContain("hunter2");
    expect(rendered).toContain("500");
  });

  it("redacts a tag whose key names a credential", () => {
    // The tag loop scrubbed values as TEXT, and `hunter2` under a `password`
    // key has no prefix and no `password=` beside it for the text rule to bite
    // on. Only the key says what it is — the same hole that was closed for
    // contexts and span data, on a surface that sweep did not reach.
    const sanitized = sanitizeEvent({
      type: undefined,
      tags: { password: "hunter2", token: "hunter2", company: "acme" },
    } as unknown as ErrorEvent);
    expect(JSON.stringify(sanitized)).not.toContain("hunter2");
    // A tag that names nothing secret is untouched, or filtering breaks.
    expect(sanitized.tags?.company).toBe("acme");
    expect(sanitized.tags?.surface).toBe("console");
  });

  it("scrubs the free-form fields that are not the message", () => {
    // Walked from `@sentry/core`'s `Event` rather than remembered. Each of
    // these was uncovered until the enumeration was made shared.
    const sanitized = sanitizeEvent({
      type: undefined,
      transaction: "GET /login?code=hunter2",
      logger: "api_key=hunter2",
      logentry: { message: "refresh failed: api_key=hunter2", params: ["password=hunter2"] },
      fingerprint: ["api_key=hunter2", "route"],
      sdkProcessingMetadata: { anything: "password=hunter2" },
    } as unknown as ErrorEvent);
    const rendered = JSON.stringify(sanitized);
    expect(rendered).not.toContain("hunter2");
    // The diagnostic around each redaction survives.
    expect(sanitized.transaction).toContain("/login");
    expect(sanitized.fingerprint).toContain("route");
  });

  it("never drops an event", () => {
    // A scrubber, not a filter: deciding which errors are worth seeing belongs
    // in the operator's own project, where they can see what they suppressed.
    expect(sanitizeEvent({ type: undefined })).not.toBeNull();
  });

  it("redacts a magic-link code from a navigation breadcrumb", () => {
    // `clearMagicLinkFromUrl()` calls `history.replaceState` AFTER the SDK's
    // history instrumentation is installed, so the breadcrumb holds the URL as
    // it was — with a working sign-in code in it.
    const sanitized = sanitizeEvent({
      type: undefined,
      breadcrumbs: [
        {
          category: "navigation",
          data: {
            from: "/login?company=acme&code=Xj7wQ2mNp4Lk9RtVb3Zc8Hy1Ds5Fg6Ae0Ui2Oq7Pw3",
            to: "/",
          },
        },
      ],
    });
    const rendered = JSON.stringify(sanitized);
    expect(rendered).not.toContain("Xj7wQ2mNp4Lk9RtVb3Zc8Hy1Ds5Fg6Ae0Ui2Oq7Pw3");
    // The route itself is the diagnostic and survives.
    expect(rendered).toContain("/login?company=acme");
  });
});

describe("sanitizeTransaction", () => {
  /** One transaction carrying something it must not send in every field. */
  function hostileTransaction(): TransactionEvent {
    return {
      type: "transaction",
      transaction: "/companies/:company/desk",
      server_name: "operator-laptop",
      user: { email: "operator@example.com" },
      request: { url: "https://console.example/?code=magic-link-code" },
      extra: { state: "password=hunter2" },
      tags: { origin: credentialShaped("th_live_", 20) },
      spans: [
        {
          span_id: "a",
          trace_id: "b",
          start_timestamp: 0,
          data: { "http.url": "https://host.example/api/v1?token=hunter2" },
          description: "GET https://u:hunter2@host.example/api/v1?code=abc123",
        },
      ],
      breadcrumbs: [{ message: "GET https://u:hunter2@host.example/api/v1" }],
      contexts: {
        trace: {
          trace_id: "ed12b4924c1b4fc3a1b87ba462a40b7c",
          span_id: "8e41772640821824",
          // Found in a captured envelope: the address bar, verbatim, while the
          // span descriptions beside it were correctly redacted.
          data: { "url.full": "http://host/?crash=1&code=magic-link-code&api_key=hunter2" },
        },
      },
    } as unknown as TransactionEvent;
  }

  it("lets no credential or identity through a transaction", () => {
    // `beforeSend` is never called for a transaction. Without this hook every
    // span would leave unscrubbed, which is a bigger surface than the events
    // the rest of this file is careful about.
    const rendered = JSON.stringify(sanitizeTransaction(hostileTransaction()));
    for (const leaked of [
      "hunter2",
      "magic-link-code",
      "abc123",
      "operator-laptop",
      "operator@example.com",
      "th_live_AAAA",
    ]) {
      expect(rendered, leaked).not.toContain(leaked);
    }
  });

  it("finds a credential nested deep inside a context", () => {
    // The shape the previous helper missed twice over: it looked only at
    // `context.data`, and only at its top level. A value directly on the
    // context and a value two levels inside `data` both went out in clear.
    const sanitized = sanitizeTransaction({
      type: "transaction",
      contexts: {
        // Directly on the context, with no `data` key at all.
        app: { token: "hunter2" },
        // And nested two levels down inside `data`.
        trace: {
          trace_id: "ed12b4924c1b4fc3a1b87ba462a40b7c",
          data: {
            request: { authorization: "Bearer hunter2" },
            list: [{ deeper: { secret: "hunter2" } }],
            "url.path": "/companies/acme",
          },
        },
      },
    } as unknown as TransactionEvent);
    const rendered = JSON.stringify(sanitized);
    expect(rendered).not.toContain("hunter2");
    // The trace id and the diagnostic beside the redaction survive.
    expect(rendered).toContain("ed12b4924c1b4fc3a1b87ba462a40b7c");
    expect(rendered).toContain("/companies/acme");
  });

  it("redacts by key where there is no text for the string rule to read", () => {
    // `hunter2` alone is a word: no issuer prefix, no `token=` beside it. Only
    // the KEY says what it is, so a scrubber that reads strings and ignores
    // structure cannot catch this at any depth.
    const sanitized = sanitizeTransaction({
      type: "transaction",
      spans: [
        {
          span_id: "a",
          trace_id: "b",
          start_timestamp: 0,
          data: { password: "hunter2", "http.status_code": 500 },
        },
      ],
      breadcrumbs: [{ data: { credentials: { anything: ["at", "any", "hunter2"] } } }],
    } as unknown as TransactionEvent);
    const rendered = JSON.stringify(sanitized);
    expect(rendered).not.toContain("hunter2");
    // A non-secret key with a non-string value is untouched.
    expect(sanitized?.spans?.[0]?.data?.["http.status_code"]).toBe(500);
  });

  it("redacts a transaction tag whose key names a credential", () => {
    const sanitized = sanitizeTransaction({
      type: "transaction",
      tags: { password: "hunter2", route: "/desk" },
      spans: [
        {
          span_id: "a",
          trace_id: "b",
          start_timestamp: 0,
          // A span carries its own tag map, with the same hole.
          tags: { secret: "hunter2" },
        },
      ],
    } as unknown as TransactionEvent);
    expect(JSON.stringify(sanitized)).not.toContain("hunter2");
    expect(sanitized?.tags?.route).toBe("/desk");
  });

  it("keeps what makes a transaction worth reading", () => {
    const sanitized = sanitizeTransaction(hostileTransaction());
    // The route template — the whole point of a transaction name.
    expect(sanitized?.transaction).toBe("/companies/:company/desk");
    expect(sanitized?.spans?.[0]?.description).toContain("host.example");
    expect(sanitized?.tags?.surface).toBe("console");
  });
});
