import { useCallback, useEffect, useRef, useState } from "react";

import type { OpenCompanyClient } from "@/api/client";
import type { TeamMemberDto } from "@/api/types";
import { useLocalScope } from "@/connections/ConnectionContext";
import { shouldOfferSetup, teamIsEmpty } from "@/lib/company-setup";
import { SetupDialog } from "./SetupDialog";
import { clearSetupSkipped, markSetupSkipped, setupSkipped } from "./state";

/**
 * Decides whether first-run setup opens, and gets out of the way once it has
 * (docs/spec/runtime/company-setup.md).
 *
 * Mounted once inside `AppShell` beside `TourController`, so it overlays every
 * view. The two are sequenced rather than independent: setup runs **first** and
 * the tour waits, because a tour of an unstaffed company walks someone through
 * empty pages — the exact first impression this feature exists to fix. The
 * `onOpenChange` callback is how the shell tells the tour to hold.
 *
 * ## The gate
 *
 * Open when the roster is empty and the operator has not skipped. The emptiness
 * test is the host's answer, not a stored flag, so it cannot drift from the thing
 * setup changes — see `shouldOfferSetup` for why a browser flag would be unsafe
 * for this and is fine for the skip.
 *
 * A company whose manifest already names agents therefore never sees the offer.
 * That is deliberate: it came with a team, so there is nothing to set up. `force`
 * is how the Team page's in-place prompt reopens it anyway.
 */
export function SetupController({
  client,
  company,
  force,
  deepLinked,
  onForceHandled,
  onOpenChange,
  onCompleted,
}: {
  client: OpenCompanyClient;
  company: string | null;
  /** Opened by hand from the Team page's prompt, regardless of the skip flag. */
  force?: boolean;
  /**
   * The operator arrived on a view they named, so do not open unprompted.
   *
   * A blocking dialog is an *offer* when someone lands on the console with
   * nowhere particular to be, and a *hijack* when they deep-linked to
   * `#/workflows/x`. They asked for that page; the Team page's in-place prompt
   * is the affordance a deliberate navigation should meet instead.
   *
   * This does not suppress `unstaffed` reporting — the tour still holds and the
   * Team prompt still shows.
   */
  deepLinked?: boolean;
  /** Clears the caller's force flag once the dialog has taken it. */
  onForceHandled?: () => void;
  /**
   * Fires whenever the tour should hold: while the dialog is open, and while the
   * company still has nobody on it.
   *
   * Emptiness and not just openness, because skipping setup would otherwise pop
   * the tour's welcome straight over an unstaffed company — a walkthrough of
   * empty pages, which is the first impression this whole feature exists to
   * replace. Held until there is a team to show.
   */
  onOpenChange?: (open: boolean) => void;
  /** Setup finished and created a team — the roster reads should refresh. */
  onCompleted?: () => void;
}) {
  const scope = useLocalScope();
  const [open, setOpen] = useState(false);
  /**
   * Whether the gate has been evaluated for this (connection, company).
   *
   * Without it a company switch would leave the previous company's answer in
   * place: the roster read is async, so the dialog would either linger over a
   * staffed company or fail to open over an empty one until the fetch landed.
   */
  const [checked, setChecked] = useState(false);
  /** Whether the host's roster is empty, independent of the skip flag. */
  const [unstaffed, setUnstaffed] = useState(false);
  /**
   * Whether the gate has already been evaluated once in this mount.
   *
   * Setup opens **unprompted only on the first evaluation**, never again on a
   * later company switch. A switch is navigation, not a first run: an operator
   * who deep-links into `#/workflows/x` on a company that happens to have no
   * team asked for that page, and a blocking modal over it is a hijack rather
   * than an offer. The Team page's in-place prompt still covers those companies,
   * which is the affordance a deliberate navigation should meet.
   *
   * A ref, not state: it must be true before the next evaluation's render, and
   * it must not itself trigger one.
   */
  const evaluatedOnce = useRef(false);

  // Report only once the roster read has landed.
  //
  // Reporting on mount would say "nothing to hold for" before we know, and the
  // tour would open its welcome in the gap — two dialogs stacked on the first
  // screen an operator ever sees. The shell therefore starts held and waits for
  // this, so the quiet case is a tour that opens a beat late rather than one
  // that flashes over setup.
  useEffect(() => {
    if (!checked) return;
    onOpenChange?.(open || unstaffed);
  }, [checked, open, unstaffed, onOpenChange]);

  // Re-evaluate the gate whenever the addressed company changes.
  useEffect(() => {
    let cancelled = false;
    setChecked(false);
    setOpen(false);

    (async () => {
      let roster: TeamMemberDto[] = [];
      try {
        roster = await client.listTeam(company);
      } catch {
        // A host with no roster surface, or one we cannot reach. Offer nothing:
        // a setup flow that cannot read the team cannot tell a fresh company
        // from a staffed one, and guessing risks a duplicate team.
        if (!cancelled) setChecked(true);
        return;
      }
      if (cancelled) return;
      const first = !evaluatedOnce.current;
      evaluatedOnce.current = true;
      setUnstaffed(teamIsEmpty(roster));
      // Only the first evaluation may open the dialog by itself; see
      // `evaluatedOnce`. Later switches still report `unstaffed`, so the tour
      // keeps holding and the Team page keeps prompting.
      setOpen(
        first && !deepLinked && shouldOfferSetup({ roster, skipped: setupSkipped(scope) }),
      );
      setChecked(true);
    })();

    return () => {
      cancelled = true;
    };
  }, [client, company, scope, deepLinked]);

  // The Team page's prompt reopens setup after a skip.
  useEffect(() => {
    if (!force) return;
    setOpen(true);
    onForceHandled?.();
  }, [force, onForceHandled]);

  const skip = useCallback(() => {
    markSetupSkipped(scope);
    setOpen(false);
  }, [scope]);

  const done = useCallback(() => {
    // Clear the skip so it cannot outlive what it was suppressing: an operator
    // who skipped, later ran setup, then removed every agent should be offered
    // setup again rather than left on an empty team page.
    clearSetupSkipped(scope);
    setOpen(false);
    // The team exists now, so the tour has something to walk through.
    setUnstaffed(false);
    onCompleted?.();
  }, [scope, onCompleted]);

  if (!checked && !force) return null;
  if (!open) return null;

  return (
    <SetupDialog open={open} client={client} company={company} onSkip={skip} onDone={done} />
  );
}
