// A single free-form value riding the hash's query suffix
// (`#/connections/mcp?permissions=notion`).
//
// The unvalidated sibling of `useHashTab`: a tab's value is one of a list the
// page owns, and an address naming something else must land on a real tab. This
// carries a name the page cannot enumerate ahead of time — which server's
// permissions are open — so the value comes back as written and the caller
// decides whether it resolves to anything.

import { useCallback, useEffect, useState } from "react";

function read(key: string): string | null {
  const [, query = ""] = window.location.hash.split("?");
  const raw = new URLSearchParams(query).get(key);
  return raw && raw.trim() ? raw : null;
}

/**
 * The value, and a setter that writes `null` as "drop the key".
 *
 * Replaces rather than pushes: opening a panel beside a list is not a
 * destination, and a Back that closed the panel one step at a time would put
 * the operator several presses away from the page they arrived from.
 *
 * Every other key is left standing — `?host=` rides the address across
 * navigations (`use-host-route.ts`), and dropping it here would strand the
 * console rendering one host under an address naming none.
 */
export function useHashParam(key: string): [string | null, (next: string | null) => void] {
  const [value, setValue] = useState(() => read(key));

  useEffect(() => {
    const onHashChange = () => setValue(read(key));
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, [key]);

  const set = useCallback(
    (next: string | null) => {
      const [path, query = ""] = window.location.hash.replace(/^#/, "").split("?");
      const params = new URLSearchParams(query);
      if (next === null) params.delete(key);
      else params.set(key, next);
      const qs = params.toString().replace(/=(?=&|$)/g, "");
      const nextHash = `#${path}${qs ? `?${qs}` : ""}`;
      if (nextHash !== window.location.hash)
        window.history.replaceState(null, "", nextHash);
      setValue(next);
    },
    [key],
  );

  return [value, set];
}
