/**
 * What an approve actually promises (issue #561).
 *
 * Approving does not resume a suspended call in place — the host refuses a
 * gated call, mints a single-use grant and re-dispatches (issue #243): a chat
 * turn re-dispatches the agent, and since issue #899 (Stage 1) a blocked
 * workflow run re-dispatches the run itself, so approving now continues that run
 * automatically rather than leaving it for a manual re-run. Since issue #469 the
 * re-dispatch happens **once per turn, when the last decision that turn was
 * blocked on lands**. So on a turn that parked four calls, three of the
 * operator's four clicks release nothing at all, and the console said "the
 * agent is completing the action" for every one of them. Measured on staging:
 * four minutes of that sentence while the step trace still read *didn't run*.
 *
 * The sentence lives here rather than in three components because all three
 * make the same claim on the operator's behalf, and a claim about what happens
 * next is exactly the kind that drifts when it is written down three times.
 */

/** How many other decisions a turn is still blocked on, as the host reports it. */
export type StillAwaiting = number | undefined;

/**
 * The confirmation for an approve.
 *
 * Three states, and the middle one is the whole point:
 *
 * * `0` — this decision released the turn, so the teammate really is picking it up;
 * * `n > 0` — it did not, and the operator is told what is still owed rather
 *   than being left to watch for work that nothing has started;
 * * `undefined` — the host predates the field, so nothing is claimed about what
 *   happens next. Silence is honest; the optimistic guess is what this fixes.
 */
export function approvedLine(stillAwaiting: StillAwaiting, detail?: string): string {
  const suffix = detail ? `: ${detail}` : "";
  if (stillAwaiting === undefined) return `Approved — recorded${suffix}`;
  if (stillAwaiting === 0) return `Approved — the teammate is picking it up now${suffix}`;
  return `Approved — waiting on ${stillAwaiting} more sign-off${
    stillAwaiting === 1 ? "" : "s"
  } before the teammate continues${suffix}`;
}

/**
 * The same answer for a card the runtime performs itself — a paused workflow
 * gate, a cold-recipient report (issue #395). There is no teammate to re-dispatch,
 * so "the teammate" must not appear; the work is still gated on the rest of the
 * turn's decisions in exactly the same way.
 */
export function approvedByRuntimeLine(stillAwaiting: StillAwaiting, detail?: string): string {
  const suffix = detail ? `: ${detail}` : "";
  if (stillAwaiting === undefined) return `Approved — recorded${suffix}`;
  if (stillAwaiting === 0) return `Approved — carrying it out now${suffix}`;
  return `Approved — waiting on ${stillAwaiting} more sign-off${
    stillAwaiting === 1 ? "" : "s"
  } before it runs${suffix}`;
}
