// The hosts this console is connected to, and which one is on screen.
//
// ## The rule this context exists to keep
//
// Host selection is a **filter over N live things**, not a selector that makes
// one live. Choosing a host changes which console is on screen and nothing
// else: no connection is torn down, no stream is closed, no storage is
// re-scoped.
//
// That is the difference between this and block/buzz's workspace rail, which
// looks the same and is not. Switching there is a stateful *apply* — it
// re-scopes the retention database, re-resolves identity and restarts the
// managed agents — because its app state holds one `relay_url_override`. The
// switcher is the visible part; the singleton behind it is why buzz cannot hold
// two workspaces at once.
//
// So: selection lives in `App`'s local state, never in the registry, and no
// code path here mutates a connection. This context only *carries* that state
// down to the switcher, which now lives in the sidebar header — two layers
// below `App`, inside a console that a host being unreachable must not be able
// to take down (issue #1142).

import { createContext, useContext, useEffect, useState, type ReactNode } from "react";

import type { LocalInstance } from "@/api/transport/desktop";
import type { Connection, ConnectionId } from "@/connections/types";

export interface HostsValue {
  connections: Connection[];
  /** The connection whose console is on screen. */
  selected: ConnectionId | null;
  /** Puts another host's console on screen. A filter — see the note above. */
  onSelect: (id: ConnectionId) => void;
  /** Registers a host reachable at `baseUrl`, and opens it. */
  onAdd: (baseUrl: string) => void;
  /**
   * The hosts this machine runs, running or not.
   *
   * Empty in a browser, which runs none, and on a shell predating the roster —
   * where the "on this computer" half simply does not draw.
   */
  localInstances: LocalInstance[];
  /** Creates a host on this machine over a data root of its own, and starts it. */
  onAddLocal?: (label: string) => Promise<void>;
  onStartLocal?: (id: string) => Promise<void>;
  onStopLocal?: (id: string) => Promise<void>;
  /** Whether this is a hub deployment, which offers the switcher at any count. */
  hub: boolean;
}

/**
 * The roster plus the one piece of UI state that must not live in the switcher.
 *
 * "Add a host" opens a dialog, and creating a host on this computer *selects*
 * it — which remounts the console the switcher is drawn inside, taking the
 * dialog with it and closing the roster mid-flow. The rail did not have this
 * problem because it stood outside the console; keeping the open flag here, and
 * the dialog mounted beside the console rather than within it, is how the
 * switcher keeps that property from its new home.
 */
export interface HostsContextValue extends HostsValue {
  /** Whether the "Add a host" dialog is open. */
  addingHost: boolean;
  setAddingHost: (open: boolean) => void;
}

const HostsContext = createContext<HostsContextValue | null>(null);

/**
 * The hosts, for anything below `App` that needs them.
 *
 * Throws rather than defaulting: a switcher rendered outside the provider would
 * silently show an empty roster, which reads as "you have no hosts" rather than
 * as the wiring mistake it is.
 */
export function useHosts(): HostsContextValue {
  const value = useContext(HostsContext);
  if (!value) throw new Error("useHosts must be used within a HostsProvider.");
  return value;
}

/** How many hosts the number row can reach. `⌘1`–`⌘9`, in list order. */
export const HOST_SHORTCUT_LIMIT = 9;

/** Whether this keyboard spells the shortcut with ⌘ rather than Ctrl. */
export function isAppleKeyboard(): boolean {
  return /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
}

/** How the shortcut for the host at `index` reads on this keyboard. */
export function hostShortcutLabel(index: number): string | null {
  if (index >= HOST_SHORTCUT_LIMIT) return null;
  return isAppleKeyboard() ? `⌘${index + 1}` : `Ctrl+${index + 1}`;
}

export function HostsProvider({ value, children }: { value: HostsValue; children: ReactNode }) {
  const { connections, onSelect } = value;
  const [addingHost, setAddingHost] = useState(false);

  // `⌘1`–`⌘9` selects the host in that position. Installed here rather than on
  // the switcher so it works in every phase — including the ones where the
  // sidebar is not mounted because the selected host is unreachable.
  //
  // **Only swallowed when a host is actually there.** With two hosts connected,
  // `⌘3` is left alone and the browser keeps its own tab shortcut; taking a key
  // to do nothing is worse than not taking it. `event.key` rather than
  // `event.code`, so a layout that puts the digits elsewhere still matches what
  // the menu prints.
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) return;
      const position = Number(event.key);
      if (!Number.isInteger(position) || position < 1 || position > HOST_SHORTCUT_LIMIT) return;
      const host = connections[position - 1];
      if (!host) return;
      event.preventDefault();
      onSelect(host.id);
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [connections, onSelect]);

  return (
    <HostsContext.Provider value={{ ...value, addingHost, setAddingHost }}>
      {children}
    </HostsContext.Provider>
  );
}
