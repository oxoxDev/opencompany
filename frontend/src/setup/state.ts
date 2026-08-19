// First-run setup's one piece of browser-local state: did this operator say
// "I'll do this later"?
//
// Keyed per (connection, company) exactly like `tour/state.ts`, so two hosts
// serving a company of the same name never share one operator's decision.
//
// ## Why a browser flag is safe here and would not be for "has setup run"
//
// `tour/state.ts` explains that first-run state lives in `localStorage` because
// `UserRecord` carries no per-user field. For the tour that is a small cost:
// cleared storage re-offers a walkthrough.
//
// Setup *creates things*, so the same trade would be unacceptable for the
// question "has setup already run?" — cleared storage would build a second team
// on top of the first. That question is therefore answered by the host instead:
// `shouldOfferSetup` asks whether the roster is empty (see
// `lib/company-setup.ts`).
//
// What lives here is only the *skip*, and skipping can do exactly one thing:
// hide an offer. Losing it re-offers setup to a company that still has nobody on
// it, which is the correct outcome anyway. So the fragile store holds the
// harmless half, and the durable store holds the half that matters.

import { type LocalScope, scopedKey } from "@/connections/types";

const KEY = (scope: LocalScope): string => scopedKey("oc-setup", scope);

interface SetupState {
  skipped?: boolean;
  at?: number;
}

function read(scope: LocalScope): SetupState {
  try {
    const raw = localStorage.getItem(KEY(scope));
    return raw ? (JSON.parse(raw) as SetupState) : {};
  } catch {
    return {};
  }
}

/** Has this operator dismissed the setup offer for this company? */
export function setupSkipped(scope: LocalScope): boolean {
  return Boolean(read(scope).skipped);
}

/** Record "I'll do this later", so the dialog stops opening by itself. */
export function markSetupSkipped(scope: LocalScope): void {
  try {
    localStorage.setItem(KEY(scope), JSON.stringify({ skipped: true, at: Date.now() }));
  } catch {
    /* private mode / quota — setup simply re-offers on the next load */
  }
}

/**
 * Forget the skip.
 *
 * Called when setup completes, so the flag cannot outlive the thing it was
 * suppressing: an operator who skips, later runs setup, and then removes every
 * agent should be offered setup again rather than silently left on an empty
 * team page.
 */
export function clearSetupSkipped(scope: LocalScope): void {
  try {
    localStorage.removeItem(KEY(scope));
  } catch {
    /* nothing to clear */
  }
}
