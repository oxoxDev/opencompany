import { lazy, Suspense, useCallback, useEffect, useState } from "react";
import type { Step, TourData } from "react-joyride";

import type { View } from "@/components/app-shell";
import { TOUR, waitForTarget } from "./steps";
import { TourTooltip } from "./TourTooltip";
import { WelcomeDialog } from "./WelcomeDialog";
import {
  clearTourResume,
  readTourResume,
  RESTART_EVENT,
  setActiveTourStop,
  tourForced,
  tourSeen,
  writeTourState,
} from "./state";
import { useLocalScope } from "@/connections/ConnectionContext";

// react-joyride (and its floater/popper deps) is a fair chunk; only operators
// who actually run the tour download it. Types above are `import type`, so they
// erase and don't pull the module eagerly.
//
// Named so it can be *started* before it is needed — see `preloadTour`.
const importJoyride = () => import("react-joyride");
const Joyride = lazy(() => importJoyride().then((m) => ({ default: m.Joyride })));

/**
 * Start downloading the tour chunk before anything renders it.
 *
 * Lazy-loading it is right — most operators never run the tour — but it puts
 * the download on the critical path of the click that starts one. "Replay tour"
 * would show a toast saying the tour was starting and then nothing visible
 * would happen until the chunk arrived, which on a cold cache or a slow
 * connection is long enough to read as a button that does not work.
 *
 * So the fetch is moved to the moment someone shows intent — pointing at the
 * button — instead of the moment they commit. By the time `session` flips and
 * `Suspense` asks for the module, the dynamic import has usually already
 * resolved and React renders it in the same frame.
 *
 * Safe to call as often as you like: `import()` caches its module promise, so
 * every call after the first is a lookup rather than a request. Failures are
 * swallowed because this is a prefetch and nothing depends on it — the real
 * `lazy` boundary will report a genuinely broken chunk when it renders.
 */
export function preloadTour(): void {
  void importJoyride().catch(() => {});
}

// react-joyride status values as string literals, so we don't statically import
// the runtime `STATUS` enum (which would defeat the lazy load).
const STATUS_FINISHED = "finished";
const STATUS_SKIPPED = "skipped";

const STEPS: Step[] = TOUR.map((s) => ({
  target: s.target,
  title: s.title,
  content: s.body,
  placement: s.placement,
  // v3 renamed `disableBeacon` → `skipBeacon`; open straight to the tooltip.
  skipBeacon: true,
  // Let react-joyride wait for the target to mount after we switch views — this
  // covers both the route swap and lazy Suspense chunks (Workflows, Usage, …),
  // so we don't need to hand-roll the wait.
  targetWaitTimeout: 6000,
  // A walkthrough shouldn't trap keyboard focus inside the tooltip.
  disableFocusTrap: true,
}));

/**
 * Owns the onboarding lifecycle: the one-time welcome dialog, then the
 * react-joyride spotlight. Mounted once inside `AppShell` (a sibling of the
 * feedback dialog) so it overlays every view and can drive `setView` itself.
 *
 * We let react-joyride own the stepping. Its `before` hook fires right before
 * each step renders — we use it to switch the console to that step's view, and
 * joyride then waits (`targetWaitTimeout`) for the target to mount before
 * spotlighting. That delegates the cross-view + lazy-Suspense timing to
 * joyride's tested machinery instead of a hand-rolled controlled `stepIndex`.
 */
export function TourController({
  company,
  setView,
  hold,
}: {
  company: string | null;
  /** `sub` names a section's sub-page, e.g. `#/settings/connections`. */
  setView: (view: View, sub?: string) => void;
  /**
   * Hold the tour back while first-run setup is on screen
   * (`docs/spec/runtime/company-setup.md`).
   *
   * The two are sequenced, not independent: a tour of an unstaffed company walks
   * an operator through empty pages, which is the first impression setup exists
   * to fix. So setup runs first and this suppresses the welcome until it closes,
   * at which point the tour has a real roster, real desks and real workflows to
   * point at.
   *
   * Only the *welcome* is held. A tour already running is left alone — setup
   * cannot open over one, because setup only opens on an empty roster and the
   * tour's own stops are what would have been empty.
   */
  hold?: boolean;
}) {
  // Which (connection, company) this subtree's browser-local state belongs to.
  const scope = useLocalScope();
  const [welcomeOpen, setWelcomeOpen] = useState(false);
  const [session, setSession] = useState(false); // mounts Joyride for the run
  const [run, setRun] = useState(false); // joyride active
  // Which stop this run opens on. `0` for a normal run; the resumed stop when
  // we came back from a full-page redirect mid-tour.
  const [startIndex, setStartIndex] = useState(0);

  // Offer the tour once per company on first arrival (or every load under the
  // dev-force flag) — unless we're returning mid-tour from a redirect, in which
  // case pick the tour back up where it left off instead of re-offering it.
  useEffect(() => {
    // A company switch must tear down the prior company's in-flight tour first,
    // otherwise `finish` would record its completion/skip under the NEW
    // company's key (cross-company contamination).
    setRun(false);
    setSession(false);
    setActiveTourStop(null);

    // Resume takes priority over the welcome dialog: the operator already
    // started the tour, left to authorize a connection, and came back. Showing
    // "Welcome to your company" from step 1 here is the bug (issue #300) — the
    // tour never recorded completed/skipped, so `tourSeen` is still false.
    const resumeView = readTourResume(scope);
    if (resumeView !== null) {
      // Consume it either way: a marker that can't be honored must not sit
      // around waiting to fire on some later, unrelated visit.
      clearTourResume(scope);
      // Match on view, not index — see `TourResume`. `findIndex` takes the
      // first stop for a view, which is unambiguous for every view that can
      // arm a marker today (only Connections navigates the page away).
      const index = TOUR.findIndex((s) => s.view === resumeView);
      if (index >= 0) {
        setStartIndex(index);
        setWelcomeOpen(false);
        setSession(true);
        setRun(true);
        return;
      }
      // -1: the stop was retired since the marker was written (as #302 retired
      // one). Drop it and fall through to today's behavior.
    }

    setStartIndex(0);
    if (tourForced() || !tourSeen(scope)) setWelcomeOpen(true);
    else setWelcomeOpen(false);
    // `hold` is deliberately NOT a dependency. This effect consumes the one-shot
    // resume marker, so re-running it eats a resume that had already been
    // honoured — which is exactly what happened when the hold was wired here.
    // The hold suppresses the *rendered* welcome instead; see below.
  }, [company, scope]);

  const start = useCallback(() => {
    // A replay from Settings always opens at the top, even if this mount had
    // resumed mid-tour.
    setStartIndex(0);
    setWelcomeOpen(false);
    setSession(true);
    setRun(true);
  }, []);

  const finish = useCallback(
    (skipped: boolean) => {
      setRun(false);
      setSession(false);
      setActiveTourStop(null);
      // Replaces the whole record, so any resume marker goes with it — the tour
      // is over, there is nothing left to resume.
      writeTourState(scope, skipped ? { skipped: true } : { completed: true });
    },
    [company, scope],
  );

  const handleSkip = useCallback(() => {
    setWelcomeOpen(false);
    writeTourState(scope, { skipped: true });
  }, [company, scope]);

  // "Replay product tour" from Settings clears the flag and dispatches
  // RESTART_EVENT; jump straight into the tour (no welcome dialog).
  useEffect(() => {
    window.addEventListener(RESTART_EVENT, start);
    return () => window.removeEventListener(RESTART_EVENT, start);
  }, [start]);

  // Before each step renders, switch the console to that step's view AND wait
  // for its target to actually be in the DOM before letting joyride proceed.
  // joyride awaits this hook, so a content anchor on a just-navigated view
  // (e.g. the chat composer on Conversation) is spotlighted only once mounted —
  // otherwise joyride checks too early, finds nothing, and skips the step.
  // `setView` is idempotent (no-op when already there), so this is safe on Back.
  const before = useCallback(
    async (data: TourData) => {
      const stop = TOUR[data.index];
      if (!stop) return;
      // Publish the live position so `armTourResume` can persist it if this
      // stop hands the browser off to a third party (the OAuth connect flow).
      setActiveTourStop(stop.view);
      setView(stop.view, stop.sub);
      await waitForTarget(stop.target);
    },
    [setView],
  );

  // End the tour (recording completed vs skipped) when joyride reports it done.
  const after = useCallback(
    (data: TourData) => {
      if (data.status === STATUS_FINISHED || data.status === STATUS_SKIPPED) {
        finish(data.status === STATUS_SKIPPED);
      }
    },
    [finish],
  );

  return (
    <>
      <WelcomeDialog
        // Held while first-run setup owns the screen, or while the company has
        // nobody on it (`docs/spec/runtime/company-setup.md`). A render-time gate
        // rather than a state one: the effect above has side effects that must
        // fire exactly once, and this only decides what is on screen.
        open={welcomeOpen && !hold}
        onOpenChange={setWelcomeOpen}
        onStart={start}
        onSkip={handleSkip}
      />
      {session && (
        <Suspense fallback={null}>
          <Joyride
            steps={STEPS}
            run={run}
            // Resume support without giving up the uncontrolled design: this is
            // the *initial* index for an uncontrolled tour, so joyride still
            // owns the stepping from here. (A controlled `stepIndex` would make
            // us drive every transition by hand.)
            initialStepIndex={startIndex}
            continuous
            tooltipComponent={TourTooltip}
            // Clamp the card into the viewport on BOTH axes (issue #583).
            //
            // joyride positions with floating-ui, and neither of its two
            // defaults can save a stop anchored to a full-viewport element —
            // which the Overview graph is (`h-svh`, top edge at y=0). Asking
            // for `placement: "top"` there resolves to `0 - height - offset`,
            // measured at y=-238: rendered, visible, mounted, and entirely
            // above the fold. Which is what gets reported as "the tour is
            // stuck on step 2".
            //
            // `flip` cannot fix it: against a full-height anchor the *bottom*
            // side overflows by exactly as much as the top, so floating-ui's
            // best-fit tie-break keeps the side it started on. And `shift`
            // clamps only its main axis, which for a top/bottom placement is
            // the horizontal one — the broken axis is the one left alone.
            //
            // Turning on `crossAxis` is the missing half: the card slides back
            // inside on the axis the placement runs along. It may then overlap
            // the element it points at, which for a full-bleed anchor is
            // unavoidable and strictly better than being unreachable.
            floatingOptions={{ shiftOptions: { crossAxis: true } }}
            options={{
              zIndex: 1200,
              overlayColor: "rgba(0,0,0,0.45)",
              spotlightPadding: 6,
              arrowSize: 0,
              before,
              after,
            }}
          />
        </Suspense>
      )}
    </>
  );
}
