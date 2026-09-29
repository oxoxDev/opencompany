// The only module in the console that imports the Sentry SDK.
//
// Everything worth testing — the decision, the scrubber, the sanitizer — lives
// in `@/lib/crash-reporting`, which imports no SDK and is covered by the unit
// suite. This file is the wiring: what is switched on, what is switched off,
// and why. See `docs/spec/runtime/crash-reporting.md`.

import * as Sentry from "@sentry/react";

import {
  SMOKE_TEST_MESSAGE,
  resolveCrashReporting,
  sanitizeEvent,
  sanitizeTransaction,
} from "@/lib/crash-reporting";

/**
 * The release tag, computed once in `vite.config.ts` and substituted at build
 * time, so the string this bundle reports and the string the source-map upload
 * files under cannot drift. A release the two disagree about is a stack trace
 * Sentry never symbolicates and no error to explain why.
 *
 * Declared in `src/vite-env.d.ts`.
 */
const RELEASE: string = __SENTRY_RELEASE__;

/**
 * Initializes crash reporting, or does nothing at all.
 *
 * Call once, before the first render, so a crash during the first render is
 * reported rather than being the thing that prevents reporting. A production
 * build reports to `VITE_SENTRY_DSN`, else to the compiled-in console project.
 * The Vite dev server never falls back to that compiled-in project — a
 * contributor's `npm run dev` must not write to it on a default nobody
 * chose — but an explicitly configured `VITE_SENTRY_DSN` still reports there
 * too. A `VITE_SENTRY_DSN=off` build, or a dev server with no DSN configured,
 * is silent: no console warning, no thrown error, no network.
 */
export function initSentry(): void {
  const config = resolveCrashReporting(import.meta.env, RELEASE);
  if (!config) return;

  Sentry.init({
    dsn: config.dsn,
    environment: config.environment,
    release: config.release,

    // No IP address and no cookies.
    sendDefaultPii: false,

    // Performance tracing: off unless the operator set a rate. See
    // `VITE_SENTRY_TRACES_SAMPLE_RATE`, and `docs/spec/runtime/crash-reporting.md`
    // for what it costs. The integration below is added on the same condition,
    // so an install that did not ask for tracing carries no instrumentation
    // rather than instrumentation that samples nothing.
    tracesSampleRate: config.tracesSampleRate,
    // Which origins may receive a `sentry-trace`/`baggage` header. Same-origin
    // only unless the operator named the host's origin — a trace header sent to
    // a third party tells them this app is instrumented and hands them an id
    // that correlates their logs with the operator's.
    tracePropagationTargets: config.tracePropagationTargets,

    // NO SESSION REPLAY, deliberately, and this is the switch that says so.
    // A replay is a DOM recording, which on this surface is the founder's chat,
    // their ledger figures and their workspace text — the exact content
    // `sanitizeEvent` exists to keep out of a report. `maskAllText` would
    // reduce it to grey boxes, which for a text console removes the reason to
    // have it. Zero as well as un-integrated, so adding an integration later
    // cannot switch sampling on by itself. See the doc for the full reasoning
    // and for what an operator who wants it anyway has to accept.
    replaysSessionSampleRate: 0,
    replaysOnErrorSampleRate: 0,

    // Every integration is opt-in. The default set includes several that
    // collect content — and two that this list has to put back, because
    // `defaultIntegrations: false` silently disables things that do not look
    // like integrations at all:
    defaultIntegrations: false,
    integrations: [
      // `ignoreErrors` below is consumed by THIS integration. Without it the
      // option is dead config that reads as a working filter.
      Sentry.inboundFiltersIntegration(),
      // `event.request.headers` comes from here, and Sentry derives `os` /
      // `browser` / `device` server-side by parsing the User-Agent in it —
      // so without this every event loses all platform context. `sanitizeEvent`
      // narrows the envelope back down to that one header.
      Sentry.httpContextIntegration(),
      // `window.onerror` and `window.onunhandledrejection`. This is what makes
      // an unhandled promise rejection a report; there is no hand-written
      // listener anywhere in this app, and there should not be.
      Sentry.globalHandlersIntegration(),
      // Wraps `setTimeout`/`requestAnimationFrame`/event listeners so a throw
      // inside one has a usable stack instead of `Script error`.
      Sentry.browserApiErrorsIntegration(),
      // `error.cause` chains, so a wrapped `ApiError` still names what failed.
      Sentry.linkedErrorsIntegration(),
      // One issue per bug rather than one per re-render.
      Sentry.dedupeIntegration(),
      Sentry.functionToStringIntegration(),
      // Breadcrumbs, narrowed to the ones that carry a shape rather than
      // content. `console` would ship anything anyone ever logged and `dom`
      // ships the text of the element clicked — which on this surface is
      // company data. What is left is method, URL and status, and
      // `sanitizeEvent` scrubs those.
      Sentry.breadcrumbsIntegration({
        console: false,
        dom: false,
        fetch: true,
        history: true,
        xhr: true,
      }),
      // Page loads, navigations and a span per request — the "what did this
      // action actually do" half of the timeline, and the half that carries a
      // `sentry-trace` header to the host so the two sides are one trace.
      // Added ONLY when a rate was asked for: `tracesSampleRate: 0` would
      // sample everything out anyway, but the integration still patches
      // `fetch`, `history` and the performance observer to do it, and an
      // install that wanted none of this should not pay for that.
      ...(config.tracesSampleRate > 0
        ? [
            Sentry.browserTracingIntegration({
              // The DOM text of the element clicked is company data; the same
              // reason `dom: false` is set on breadcrumbs above.
              enableInp: false,
            }),
          ]
        : []),
    ],

    beforeSend: sanitizeEvent,
    // A SEPARATE hook: `beforeSend` is never called for a transaction, so
    // without this every span would bypass the scrubber entirely.
    beforeSendTransaction: sanitizeTransaction,

    // Non-actionable browser noise. Every one of these is something the page
    // recovers from on its own: a layout loop the browser already broke, a
    // request the user navigated away from, an extension's fetch shim.
    ignoreErrors: [
      "ResizeObserver loop",
      "Network request failed",
      "Failed to fetch",
      "Load failed",
      "AbortError",
    ],
  });

  // A rate that was set and could not be read is worth one line, on the same
  // reasoning as the host's `Silence::Unreadable`: an operator who typed `50%`
  // must be able to tell their typo from a working default. Only ever reached
  // on an install that configured a DSN, so it cannot become noise in a local
  // checkout or in CI.
  if (config.tracesUnreadable) {
    console.warn(
      "VITE_SENTRY_TRACES_SAMPLE_RATE is not a number between 0 and 1; " +
        "performance tracing is off.",
    );
  }

  // The one-shot pipeline check. Set `VITE_SENTRY_SMOKE_TEST=true` for a single
  // build, load the console once, and look for this message in Sentry: that
  // proves the DSN, the release tag and the source-map upload all work,
  // without waiting for something to break. Then unset it.
  if (config.smokeTest) {
    Sentry.captureMessage(SMOKE_TEST_MESSAGE, "info");
  }
}

/**
 * Whether a client is installed, i.e. whether an event captured now would
 * actually be sent.
 *
 * The crash screen asks before showing the event id `Sentry.ErrorBoundary`
 * hands it. The SDK mints that id locally whether or not reporting is on, so an
 * install with no DSN would otherwise offer the operator a reference that
 * exists nowhere — and they would spend a support round trip finding that out.
 */
export function isReporting(): boolean {
  return Sentry.getClient() !== undefined;
}
