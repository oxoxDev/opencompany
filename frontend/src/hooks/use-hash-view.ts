import { useCallback, useEffect, useState } from "react";

const readHash = (): string => window.location.hash.replace(/^#\/?/, "").split(/[/?]/)[0];

/**
 * A tiny hash router: keeps the active view in `location.hash` (e.g.
 * `#/conversation`) so views are linkable, survive refresh, and honor
 * back/forward — without pulling in a full router or disturbing the app's
 * boot phases. Falls back to `fallback` for unknown/empty hashes.
 */
export function useHashView<T extends string>(
  valid: readonly T[],
  fallback: T,
): [T, (view: T) => void] {
  const resolve = useCallback(
    (): T => {
      const h = readHash();
      return (valid as readonly string[]).includes(h) ? (h as T) : fallback;
    },
    [valid, fallback],
  );

  const [view, setView] = useState<T>(resolve);

  /**
   * Rewrite the URL so it names the view actually on screen. An empty hash and
   * an unknown one (`#/finances` after a surface is retired, a typo, a stale
   * bookmark) both resolve to `fallback`, and without this the address bar
   * keeps claiming a view that isn't rendered.
   *
   * Replace semantics, never push: pushing leaves the unknown hash in the
   * history stack, so Back returns to it, this rewrite bounces forward again,
   * and the operator is stuck in a ping-pong they cannot Back out of.
   */
  const canonicalize = useCallback((next: T) => {
    if (readHash() === next) return;
    window.history.replaceState(null, "", `#/${next}`);
  }, []);

  // Reflect the resolved view into the URL when the page arrived with no hash
  // or an unrecognized one.
  useEffect(() => {
    canonicalize(view);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Follow browser back/forward and manual hash edits.
  useEffect(() => {
    const onHash = () => {
      const next = resolve();
      setView(next);
      canonicalize(next);
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, [resolve, canonicalize]);

  const navigate = useCallback((next: T) => {
    if (readHash() !== next) window.location.hash = `/${next}`;
    setView(next);
  }, []);

  return [view, navigate];
}
