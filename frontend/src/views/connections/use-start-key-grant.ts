import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";

import type { OpenCompanyClient } from "@/api/client";
import { getCompanyCredential, startCredentialLink } from "@/api/credential";
import { ApiError } from "@/api/types";
import { openOutward } from "@/lib/external-links";

/** How often the desktop asks whether the grant has landed on the host. */
const POLL_MS = 2_000;
/** How long it keeps asking — comfortably past the hub's own 10-minute code. */
const POLL_FOR_MS = 15 * 60 * 1_000;

/**
 * Starts a one-click TinyHumans key grant (`POST …/credential/link/start`)
 * and gets the person to the hub, wherever the console is running.
 *
 * Two return legs, decided by where the console is:
 *
 * - **A browser tab.** The hub URL is a top-level navigation; the hub comes
 *   back to this console's own origin with `?key=link&state=&code=`, `App`
 *   captures the code, and {@link useRedeemKeyGrant} spends it. Nothing here
 *   survives the navigation, which is the point.
 * - **The desktop.** The webview cannot host a sign-in — the hub needs its own
 *   address bar visible — so the URL is handed to the system browser, and the
 *   host redeems the grant itself on its own return route
 *   (`GET /auth/key/callback`), because nothing in that browser can reach this
 *   console. This hook then polls the credential status until the key lands
 *   and calls `onConnected`, so the page the person clicked on is the page
 *   that says so.
 *
 * Returns the start action and whether a grant is in flight.
 */
export function useStartKeyGrant(
  client: OpenCompanyClient,
  company: string | null,
  onConnected: () => void,
): { start: () => void; starting: boolean } {
  const [starting, setStarting] = useState(false);
  // The latest callbacks, read when the poll completes rather than closed
  // over when it started, for the same reason `useRedeemKeyGrant` does it.
  const latest = useRef({ client, company, onConnected });
  latest.current = { client, company, onConnected };
  const mounted = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const start = useCallback(async () => {
    setStarting(true);
    let before: string | null = null;
    try {
      // What "landed" means: the status changing hands. Read first so a key
      // already stored is not mistaken for the one this grant is minting.
      try {
        const status = await getCompanyCredential(client, company);
        before = `${status.configured}:${status.source}`;
      } catch {
        // Unreadable now is not a reason to refuse the grant; the poll below
        // then treats any readable `configured` as the landing.
      }
      const { authorizeUrl } = await startCredentialLink(client, company);
      if (!openOutward(authorizeUrl)) {
        window.location.assign(authorizeUrl);
        return;
      }
    } catch (err) {
      toast.error(
        err instanceof ApiError ? err.message : "Couldn't start the TinyHumans connection.",
      );
      setStarting(false);
      return;
    }

    toast.info("Finish connecting in your browser — this page will update when it lands.");
    const deadline = Date.now() + POLL_FOR_MS;
    const tick = async () => {
      if (!mounted.current) return;
      const { client: now, company: scope, onConnected: done } = latest.current;
      if (now !== client || scope !== company) return;
      try {
        const status = await getCompanyCredential(now, scope);
        const after = `${status.configured}:${status.source}`;
        if (status.configured && after !== before) {
          setStarting(false);
          toast.success("Connected to TinyHumans.");
          done();
          return;
        }
      } catch {
        // A transient read failure is not the grant failing; keep asking.
      }
      if (Date.now() >= deadline) {
        setStarting(false);
        return;
      }
      window.setTimeout(() => void tick(), POLL_MS);
    };
    window.setTimeout(() => void tick(), POLL_MS);
  }, [client, company]);

  return { start: () => void start(), starting };
}
