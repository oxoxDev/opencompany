// @vitest-environment jsdom

import { beforeEach, describe, expect, it } from "vitest";

import { OpenCompanyClient } from "@/api/client";
import type {
  StreamHandlers,
  Transport,
  TransportRequest,
  TransportResponse,
} from "@/api/transport";
import { needsCarriedSession } from "@/api/transport";
import {
  addConnection,
  adoptSession,
  getConnection,
  resetConnections,
} from "@/connections/registry";
import { readProfiles } from "@/connections/profileStore";
import { connectionConfig } from "@/connections/types";
import { hostSwitcherInteractive } from "@/components/host-switcher";

/**
 * A **hub** console: one deployment of this app operating hosts that live on
 * other origins, reached as subdomains.
 *
 * The whole feature turns on one fact — a cross-origin console never receives
 * the session cookie, because the host sets it `SameSite=Lax`. So it asks for a
 * token and carries it itself. These tests pin the three places that can
 * silently undo that:
 *
 *   1. *deciding* a connection needs to carry one (get it wrong and a console
 *      either drops its cookie for no reason, or holds a token it never sends);
 *   2. *sending* it, on requests and on the event stream alike;
 *   3. *keeping* it across a reload, which is the only reason it is persisted.
 *
 * Each failure is quiet. None of them throws — they produce a console that
 * signs in successfully and is anonymous immediately afterwards.
 */

/** Records what it was handed, and answers with whatever the test staged. */
class StubTransport implements Transport {
  readonly seen: TransportRequest[] = [];
  readonly streams: Array<{ url: string; headers?: Record<string, string> }> = [];

  constructor(private readonly text: string = "{}") {}

  async request(req: TransportRequest): Promise<TransportResponse> {
    this.seen.push(req);
    return {
      status: 200,
      statusText: "",
      url: req.url,
      text: this.text,
      header: () => null,
    };
  }

  subscribe(
    url: string,
    _handlers: StreamHandlers,
    headers?: Record<string, string>,
  ): () => void {
    this.streams.push({ url, headers });
    return () => {};
  }
}

function clientWith(session: string | null, transport: Transport): OpenCompanyClient {
  return new OpenCompanyClient(
    {
      baseUrl: "https://acme.example.com",
      company: null,
      operatorToken: null,
      sessionHeader: session,
    },
    transport,
  );
}

beforeEach(() => {
  resetConnections();
  window.localStorage.clear();
});

describe("deciding which connections carry their own session", () => {
  it("leaves a same-origin console on its cookie", () => {
    // The cookie is strictly better wherever it works — nothing in the page can
    // read it — so the carried token must never be chosen when it is available.
    expect(needsCarriedSession("")).toBe(false);
    expect(needsCarriedSession(window.location.origin)).toBe(false);
  });

  it("carries its own session to another origin", () => {
    // The case the hub is built out of: `app.example.com` operating
    // `acme.example.com`. A shared parent domain does not make these same-origin
    // and does not make `SameSite=Lax` send the cookie.
    expect(needsCarriedSession("https://acme.example.com")).toBe(true);
    expect(needsCarriedSession("https://other.test")).toBe(true);
  });

  it("treats a different port on this host as another origin", () => {
    // The classic near-miss: an origin is scheme + host + *port*, and a cookie
    // is not shared across ports any more than across hosts.
    const elsewhere = `${window.location.protocol}//${window.location.hostname}:31337`;
    expect(needsCarriedSession(elsewhere)).toBe(true);
  });
});

describe("sending a carried session", () => {
  it("puts it on every request", async () => {
    const t = new StubTransport();
    await clientWith("acme.tok", t).get("/api/v1/company/status");
    expect(t.seen[0].headers["x-opencompany-session"]).toBe("acme.tok");
  });

  it("puts it on the event stream too", () => {
    // The subtlest way to get this wrong. A stream that authenticates
    // differently from the requests beside it loads every view correctly and
    // then never updates one — which reads as a quiet company, not as an
    // unauthorized client.
    const t = new StubTransport();
    clientWith("acme.tok", t).subscribeToEvents(null, { onMessage: () => {} });
    expect(t.streams[0].headers?.["x-opencompany-session"]).toBe("acme.tok");
  });

  it("sends nothing extra when there is no carried session", async () => {
    // A same-origin console must be byte-identical to what it was before this
    // existed, or the cookie deployment starts depending on the hub's code path.
    const t = new StubTransport();
    await clientWith(null, t).get("/api/v1/company/status");
    expect(t.seen[0].headers["x-opencompany-session"]).toBeUndefined();
  });

  it("asks for the header carrier only when signing in, and only cross-origin", async () => {
    // The carrier header is an assertion about this client. Sending it on
    // ordinary calls would make every request carry a claim it has no business
    // making; never sending it would mean a hub sign-in silently mints a cookie
    // it cannot receive.
    const t = new StubTransport();
    const client = clientWith(null, t);
    await client.postSignIn("/api/v1/company/auth/verify", { code: "x" });
    await client.post("/api/v1/company/chat", { message: "hi" });

    expect(t.seen[0].headers["x-opencompany-session-carrier"]).toBe("header");
    expect(t.seen[1].headers["x-opencompany-session-carrier"]).toBeUndefined();
  });

  it("does not ask for the header carrier on a same-origin sign-in", async () => {
    const t = new StubTransport();
    const client = new OpenCompanyClient(
      { baseUrl: "", company: null, operatorToken: null, sessionHeader: null },
      t,
    );
    await client.postSignIn("/api/v1/company/auth/verify", { code: "x" });
    expect(t.seen[0].headers["x-opencompany-session-carrier"]).toBeUndefined();
  });
});

describe("keeping a carried session", () => {
  it("survives a reload", () => {
    // The only reason this token is persisted at all. Without it a hub asks for
    // a fresh sign-in to every host on every page load, which is not a console
    // anyone keeps using.
    const id = addConnection({ baseUrl: "https://acme.example.com" });
    adoptSession(id, "acme.tok");

    const stored = readProfiles().find((p) => p.id === id);
    expect(stored?.credential).toEqual({ kind: "session", value: "acme.tok" });
  });

  it("reaches the client that a connection's requests go through", () => {
    const id = addConnection({ baseUrl: "https://acme.example.com" });
    adoptSession(id, "acme.tok");

    const connection = getConnection(id);
    expect(connection).toBeDefined();
    expect(connectionConfig(connection!).sessionHeader).toBe("acme.tok");
  });

  it("replaces the previous session rather than keeping it", () => {
    // Signing in again mints a new token and the old one stops working. A
    // client left holding the previous value keeps working until that one
    // expires, so the bug surfaces long after the change that caused it.
    const id = addConnection({ baseUrl: "https://acme.example.com" });
    adoptSession(id, "acme.first");
    adoptSession(id, "acme.second");

    expect(getConnection(id)?.credential).toEqual({
      kind: "session",
      value: "acme.second",
    });
  });

  it("refuses to send a session over plain http to a remote host", () => {
    // The token is a person's standing authority on a company. Unlike a cookie,
    // this page holds it and puts it on the wire itself, so `Secure` is not
    // doing this job for us.
    const id = addConnection({ baseUrl: "http://acme.example.com" });
    adoptSession(id, "acme.tok");

    return probeRefuses(id);
  });
});

/** Probing an insecurely credentialed connection must fail it, not attempt it. */
async function probeRefuses(id: string): Promise<void> {
  const { probe } = await import("@/connections/registry");
  await probe(id);
  const connection = getConnection(id);
  expect(connection?.status).toBe("down");
  expect(connection?.error).toMatch(/not encrypted/);
}

describe("the host switcher in a hub", () => {
  it("opens a menu at any count, including none", () => {
    // A hub has no bootstrap connection, so it can genuinely hold zero hosts —
    // and "Add a host" in this menu is the only way to add the first one. An
    // inert nameplate there is a dead end with no way out of it.
    expect(hostSwitcherInteractive(0, true)).toBe(true);
    expect(hostSwitcherInteractive(1, true)).toBe(true);
  });

  it("still stays out of the way of an ordinary single-host console", () => {
    // One host in a browser is a nameplate, not a control: no chevron, and
    // nothing to open. Same rule the rail used for whether to draw at all.
    expect(hostSwitcherInteractive(1, false)).toBe(false);
    expect(hostSwitcherInteractive(2, false)).toBe(true);
  });
});
