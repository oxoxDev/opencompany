import { useEffect, useState } from "react";

import { me as fetchMe } from "@/api/auth";
import type { OpenCompanyClient } from "@/api/client";

/**
 * Whether the signed-in viewer may administer this company.
 *
 * Courtesy, never enforcement: every write behind an admin-only control answers
 * `403 only an admin can do that` whatever this returns. What it decides is
 * whether the console *offers* the control at all.
 *
 * # Why this is a hook and not eight copies of an effect
 *
 * It was eight copies of an effect. Ten, counting the surfaces outside Settings.
 * Each one resolved the role correctly — and resolving it was never the part
 * that went wrong. Two pages resolved it and then wired the answer into one
 * sub-component while their own credential form, Save and Disconnect went on
 * rendering enabled for a member; two more never asked at all. A page could be
 * half-gated because "know the role" and "use the role" were separate acts, and
 * nothing connected them.
 *
 * One definition does not by itself close that gap — a caller can still ignore
 * what it returns. What it does is make the gate cheap enough that there is no
 * reason to reach for half of it, and put the question in one place where the
 * answer, and this warning, are read together.
 *
 * # Why it fails closed
 *
 * `false` until the read answers, and `false` if it never does. An unresolved
 * role must not render an enabled button: the cost of being briefly wrong that
 * way is an admin seeing a read-only notice for one round trip, and the cost of
 * being wrong the other way is inviting someone to paste a live credential into
 * a form that can only refuse it.
 *
 * A host with no user plane, or a signed-out console, lands here too. Both are
 * non-admin for this purpose.
 */
export function useCanManage(client: OpenCompanyClient, company: string | null): boolean {
  const [canManage, setCanManage] = useState(false);

  useEffect(() => {
    let live = true;
    // Re-closed on every company change, not just on the first read: carrying a
    // previous company's `true` across the switch would offer an admin's
    // controls on a company this person may only be a member of.
    setCanManage(false);
    void (async () => {
      let admin = false;
      try {
        admin = (await fetchMe(client, company)).role === "admin";
      } catch {
        // No user plane on this host, or not signed in.
      }
      if (live) setCanManage(admin);
    })();
    return () => {
      live = false;
    };
  }, [client, company]);

  return canManage;
}
