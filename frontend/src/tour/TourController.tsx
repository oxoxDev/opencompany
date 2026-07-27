import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import type { Step, TourData } from "react-joyride";

import type { View } from "@/components/app-shell";
import { TOUR, waitForTarget } from "./steps";
import { TourTooltip } from "./TourTooltip";
import { WelcomeDialog } from "./WelcomeDialog";
import { RESTART_EVENT, tourForced, tourSeen, writeTourState } from "./state";

// react-joyride (and its floater/popper deps) is a fair chunk; only operators
// who actually run the tour download it. Types above are `import type`, so they
// erase and don't pull the module eagerly.
const Joyride = lazy(() => import("react-joyride").then((m) => ({ default: m.Joyride })));

// react-joyride status/action values as string literals, so we don't statically
// import the runtime `STATUS`/`ACTIONS` enums (which would defeat the lazy load).
const STATUS_FINISHED = "finished";
const STATUS_SKIPPED = "skipped";
const ACTION_PREV = "prev";
const ACTION_SKIP = "skip";
const ACTION_CLOSE = "close";

const STEPS: Step[] = TOUR.map((s) => ({
  target: s.target,
  title: s.title,
  content: s.body,
  placement: s.placement,
  disableBeacon: true,
}));

/**
 * Owns the onboarding lifecycle: the one-time welcome dialog, then a controlled
 * react-joyride spotlight that navigates the console view-by-view. Mounted once
 * inside `AppShell` (a sibling of the feedback dialog) so it overlays every view
 * and can drive `setView` itself.
 *
 * The crux is cross-view stepping against a hash router with lazy views: on each
 * Back/Next we pause the tour (`active=false`), switch the pane, wait for the
 * next target to actually mount, then resume at the new index — so the spotlight
 * never anchors on a stale or not-yet-mounted node.
 */
export function TourController({
  company,
  view,
  setView,
}: {
  company: string | null;
  view: View;
  setView: (view: View) => void;
}) {
  const [welcomeOpen, setWelcomeOpen] = useState(false);
  const [session, setSession] = useState(false); // whole tour lifetime (mounts Joyride)
  const [active, setActive] = useState(false); // run prop (paused during nav)
  const [stepIndex, setStepIndex] = useState(0);
  // True only while we're navigating+waiting between steps, so the
  // external-navigation guard doesn't fire on our own view switch.
  const transitioning = useRef(false);

  // Offer the tour once per company on first arrival (or every load under the
  // dev-force flag). Runs when the console mounts / the company changes.
  useEffect(() => {
    if (tourForced() || !tourSeen(company)) setWelcomeOpen(true);
    else setWelcomeOpen(false);
  }, [company]);

  const finish = useCallback(
    (skipped: boolean) => {
      transitioning.current = false;
      setActive(false);
      setSession(false);
      setStepIndex(0);
      writeTourState(company, skipped ? { skipped: true } : { completed: true });
    },
    [company],
  );

  // Land on a given step: pause, switch view if needed, wait for the target to
  // mount, then resume there. A target that never mounts ends the tour cleanly.
  const goTo = useCallback(
    async (nextIndex: number) => {
      const stop = TOUR[nextIndex];
      if (!stop) {
        finish(false);
        return;
      }
      transitioning.current = true;
      setActive(false);
      setView(stop.view);
      const ok = await waitForTarget(stop.target);
      transitioning.current = false;
      if (!ok) {
        finish(false);
        return;
      }
      setStepIndex(nextIndex);
      setActive(true);
    },
    [setView, finish],
  );

  const start = useCallback(async () => {
    setSession(true);
    setStepIndex(0);
    await goTo(0);
  }, [goTo]);

  const handleStart = useCallback(() => {
    setWelcomeOpen(false);
    void start();
  }, [start]);

  const handleSkip = useCallback(() => {
    setWelcomeOpen(false);
    writeTourState(company, { skipped: true });
  }, [company]);

  // "Replay product tour" from Settings clears the flag and dispatches
  // RESTART_EVENT; jump straight into the tour (no welcome dialog).
  useEffect(() => {
    const onRestart = () => {
      setWelcomeOpen(false);
      void start();
    };
    window.addEventListener(RESTART_EVENT, onRestart);
    return () => window.removeEventListener(RESTART_EVENT, onRestart);
  }, [start]);

  // If the operator navigates away mid-tour (sidebar click, browser back), the
  // spotlight would point at the wrong pane — end the tour quietly instead.
  useEffect(() => {
    if (!active || transitioning.current) return;
    if (view !== TOUR[stepIndex]?.view) finish(true);
  }, [view, active, stepIndex, finish]);

  // react-joyride v3 fires this `after` hook once per completed step, carrying
  // the action the operator took. We control `stepIndex`, so we translate that
  // into a navigate-then-wait `goTo` (or end the tour on skip/close/finish).
  const handleAfter = useCallback(
    (data: TourData) => {
      const { status, action, index } = data;
      const skipped = action === ACTION_SKIP || status === STATUS_SKIPPED;
      if (skipped || action === ACTION_CLOSE || status === STATUS_FINISHED) {
        finish(skipped);
        return;
      }
      void goTo(action === ACTION_PREV ? index - 1 : index + 1);
    },
    [finish, goTo],
  );

  return (
    <>
      <WelcomeDialog
        open={welcomeOpen}
        onOpenChange={setWelcomeOpen}
        onStart={handleStart}
        onSkip={handleSkip}
      />
      {session && (
        <Suspense fallback={null}>
          <Joyride
            steps={STEPS}
            run={active}
            stepIndex={stepIndex}
            continuous
            tooltipComponent={TourTooltip}
            options={{
              zIndex: 1200,
              overlayColor: "rgba(0,0,0,0.45)",
              spotlightPadding: 6,
              arrowSize: 0,
              after: handleAfter,
            }}
          />
        </Suspense>
      )}
    </>
  );
}
