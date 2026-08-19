import { describe, expect, it } from "vitest";

import { hostSwitcherInteractive, worstStatus } from "@/components/host-switcher";
import { hostShortcutLabel, HOST_SHORTCUT_LIMIT } from "@/connections/HostsContext";
import type { Connection, ConnectionStatus } from "@/connections/types";

/**
 * The switcher's trigger is the only place cross-host health survives.
 *
 * The rail it replaced (issue #1142) showed one dot per host, permanently. A
 * dropdown hides its rows, so an operator running three hosts learns nothing
 * about the two they are not looking at unless the trigger says so — and what
 * it says is this function. Getting the ordering wrong is silent: a console
 * that reports "Connected" while a host is unreachable looks exactly like a
 * console where everything is fine.
 */
function host(status: ConnectionStatus, id: string = status): Connection {
  return {
    id,
    defaultCompany: null,
    label: id,
    baseUrl: "",
    credential: { kind: "cookie" },
    status,
    identity: null,
    companies: [],
  };
}

describe("worstStatus", () => {
  it("has nothing to report with no hosts", () => {
    expect(worstStatus([])).toBeNull();
  });

  it("stays quiet while every host is live", () => {
    expect(worstStatus([host("live", "a"), host("live", "b")])).toBe("live");
  });

  it("reports the unreachable host, not the one on screen", () => {
    // THE case. The host being viewed is fine; the trigger must still say that
    // something, somewhere, is not.
    expect(worstStatus([host("live"), host("down")])).toBe("down");
  });

  it("prefers a host that is gone over one that is merely refusing", () => {
    expect(worstStatus([host("unauthenticated"), host("down")])).toBe("down");
    expect(worstStatus([host("degraded"), host("unauthenticated")])).toBe("unauthenticated");
  });

  it("does not claim everything is fine while a roster is still settling", () => {
    expect(worstStatus([host("live"), host("connecting")])).toBe("connecting");
  });
});

describe("hostSwitcherInteractive", () => {
  it("is a nameplate on an ordinary single-host browser console", () => {
    expect(hostSwitcherInteractive(1, false)).toBe(false);
  });

  it("becomes a control as soon as there is a choice", () => {
    expect(hostSwitcherInteractive(2, false)).toBe(true);
  });

  it("opens at any count on a hub, which has no bootstrap host to fall back on", () => {
    expect(hostSwitcherInteractive(0, true)).toBe(true);
  });
});

describe("hostShortcutLabel", () => {
  it("numbers the first nine hosts from one", () => {
    expect(hostShortcutLabel(0)).toMatch(/1$/);
    expect(hostShortcutLabel(HOST_SHORTCUT_LIMIT - 1)).toMatch(/9$/);
  });

  it("prints nothing past the number row, so no row promises a key that does nothing", () => {
    expect(hostShortcutLabel(HOST_SHORTCUT_LIMIT)).toBeNull();
  });
});
