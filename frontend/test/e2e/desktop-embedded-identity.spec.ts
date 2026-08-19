import { expect, test, type Page } from "@playwright/test";

import { openHostMenu } from "./host-switcher";

/**
 * Issue #615 — one "This computer" row, however many times the app restarts.
 *
 * The embedded host binds an ephemeral port on purpose, so its address is
 * different on every launch. The console recognised a host *by* that address,
 * so every launch read as a first meeting: a new connection id, a new row, and
 * the previous launch's row left behind pointing at a closed port. The registry
 * is durable, so they accumulated — all labelled alike, all but one broken.
 *
 * ## What is real here and what is not
 *
 * Everything the bug lives in is real: the built console, a live host, a real
 * browser, real `localStorage`, and a real page load per simulated launch. The
 * unit tests drive `adoptEmbeddedHost` directly; this drives the whole app
 * through the same door the desktop uses, which is the only way to catch the
 * bug re-entering through `App`'s wiring rather than the registry's.
 *
 * The one shimmed part is the Tauri bridge, and it has to be: `window.__TAURI__`
 * exists only inside the packaged shell, and no browser can produce one. The
 * shim below is not a mock of the behaviour under test — it answers
 * `oc_embedded` with an address and an identity, and proxies `oc_request` to
 * `fetch`, which is what the Rust core does. Every decision this spec asserts
 * on is made by the console's own code.
 *
 * The identity the shim reports is read from the host's own `/spec`, not
 * invented, because in the real desktop both come from one instance over one
 * data root. A fixture that let them differ would be testing a state that
 * cannot occur.
 *
 * ## A page load is a launch
 *
 * The init script advances a counter in `localStorage` and hands out the next
 * address, so `page.reload()` is a relaunch: new port, same machine. The last
 * address is the live host, so the connection that survives the sequence is one
 * that genuinely answers — the difference between "the rows collapsed" and "the
 * row that remains is the working one".
 */

/** Ports nothing is listening on — a previous launch's, after the app quit. */
const CLOSED_PORTS = ["http://127.0.0.1:65145", "http://127.0.0.1:65275"];

/** The persisted half of a connection, as far as this spec is concerned. */
interface Profile {
  id: string;
  baseUrl: string;
  label: string;
  instanceId?: string;
  origin?: string;
}

interface Fixture {
  /** One per launch; the last one repeats if the app is loaded again. */
  addresses: string[];
  instanceId: string;
  /** Written on the first load only, to stand in for an older version's state. */
  seed?: Profile[];
}

/**
 * Installs a Tauri bridge over the page, the way the packaged shell would.
 *
 * `oc_request` proxies to `fetch` rather than pretending to answer: the console
 * routes *every* call through the bridge once one exists, including the
 * bootstrap connection's, so a shim that faked responses would be testing
 * itself. Same-origin here, so the browser's own credentials carry the session
 * exactly as the core's would.
 */
async function installDesktopShell(page: Page, fixture: Fixture): Promise<void> {
  await page.addInitScript((f: Fixture) => {
    // The tour modal covers the console and swallows clicks.
    for (const key of ["oc-tour:single", "oc-tour:e2e-harness-co", "oc-tour:null"]) {
      window.localStorage.setItem(key, JSON.stringify({ skipped: true, seenAt: Date.now() }));
    }

    // An older version's registry, once, before the app has run at all.
    if (f.seed && !window.localStorage.getItem("e2e.desktop.seeded")) {
      window.localStorage.setItem("e2e.desktop.seeded", "1");
      window.localStorage.setItem("oc.connections.v1", JSON.stringify(f.seed));
    }

    // Which launch this is. The counter is what makes `reload()` a relaunch.
    const launch = Number(window.localStorage.getItem("e2e.desktop.launch") ?? "0");
    window.localStorage.setItem("e2e.desktop.launch", String(launch + 1));
    const baseUrl = f.addresses[Math.min(launch, f.addresses.length - 1)];

    /** Where the core has been told each connection lives. */
    const hosts = new Map<string, string>();

    async function proxy(
      connectionId: string,
      request: {
        method: string;
        path: string;
        headers?: Record<string, string>;
        body?: string | null;
      },
    ) {
      const base = hosts.get(connectionId) ?? "";
      const response = await fetch(`${base}${request.path}`, {
        method: request.method,
        headers: request.headers,
        body: request.body ?? undefined,
        credentials: "include",
      });
      // Rust lowercases every key on the way out, and `ProxyTransport` looks
      // them up that way.
      const headers: Record<string, string> = {};
      response.headers.forEach((value, key) => {
        headers[key.toLowerCase()] = value;
      });
      return {
        status: response.status,
        statusText: response.statusText,
        url: response.url,
        text: await response.text(),
        headers,
      };
    }

    (window as unknown as { __TAURI__: unknown }).__TAURI__ = {
      // Tauri v2 namespaces the API: `withGlobalTauri` injects the whole
      // `@tauri-apps/api` bundle, and `invoke`/`Channel` live under `core`. A shim
      // that puts them at the top level is the v1 shape — the one the console
      // itself used to read (#616) — so a spec built on it would drive a bridge
      // the real app can never resolve.
      core: {
        invoke(command: string, args: Record<string, string> = {}) {
          switch (command) {
            // The roster, which is what a current console asks first. One
            // instance, rooted at the data dir — the shape every machine has
            // before anyone adds a second company.
            case "oc_local_instances":
              return Promise.resolve([
                {
                  id: "default",
                  label: "This computer",
                  dataDir: "/tmp/e2e-embedded",
                  running: true,
                  baseUrl,
                  instanceId: f.instanceId,
                  companies: [],
                },
              ]);
            case "oc_embedded":
              return Promise.resolve({
                baseUrl,
                dataDir: "/tmp/e2e-embedded",
                instanceId: f.instanceId,
              });
            case "oc_connect":
              hosts.set(args.connectionId, args.baseUrl);
              return Promise.resolve();
            case "oc_disconnect":
              hosts.delete(args.connectionId);
              return Promise.resolve();
            case "oc_connections":
              return Promise.resolve([...hosts.keys()]);
            case "oc_request":
              return proxy(
                args.connectionId,
                args.request as unknown as Parameters<typeof proxy>[1],
              );
            // Opens and stays quiet. The console's poll is the safety net, and a
            // stream that errors would exercise a reconnect path this spec is
            // not about.
            case "oc_subscribe":
              return Promise.resolve();
            default:
              return Promise.resolve(undefined);
          }
        },
        Channel: class {
          onmessage: unknown = null;
        },
      },
    };
  }, fixture);
}

/** Every persisted connection, straight out of the store the app writes. */
async function profiles(page: Page): Promise<Profile[]> {
  return page.evaluate(() => {
    const raw = window.localStorage.getItem("oc.connections.v1");
    return raw ? (JSON.parse(raw) as Profile[]) : [];
  });
}

async function embedded(page: Page): Promise<Profile[]> {
  return (await profiles(page)).filter((p) => p.origin === "embedded");
}

/** The host's own identity, which is what the shell reports for a real launch. */
async function instanceIdOf(request: { get: (url: string) => Promise<{ json: () => Promise<unknown> }> }) {
  const spec = (await (await request.get("/spec")).json()) as { instance_id?: string };
  expect(spec.instance_id, "the host must report an instance id").toBeTruthy();
  return spec.instance_id as string;
}

test("a relaunch at a new port re-addresses the connection instead of adding one", async ({
  page,
  request,
  baseURL,
}) => {
  const instanceId = await instanceIdOf(request);
  const live = (baseURL ?? "").replace(/\/$/, "");
  await installDesktopShell(page, {
    addresses: [...CLOSED_PORTS, live],
    instanceId,
  });

  // Launch 1.
  await page.goto("/#/ledgers/tasks");
  await expect.poll(async () => (await embedded(page))[0]?.baseUrl, { timeout: 30_000 }).toBe(
    CLOSED_PORTS[0],
  );
  const first = (await embedded(page))[0];

  // Launch 2 — a different port, the same machine.
  await page.reload();
  await expect.poll(async () => (await embedded(page))[0]?.baseUrl, { timeout: 30_000 }).toBe(
    CLOSED_PORTS[1],
  );
  expect((await embedded(page))[0].id, "the connection id must survive a relaunch").toBe(
    first.id,
  );

  // Launch 3 — the address that actually answers.
  await page.reload();
  await expect.poll(async () => (await embedded(page))[0]?.baseUrl, { timeout: 30_000 }).toBe(
    live,
  );

  // THE assertion. Three launches, three addresses, one row — where before this
  // there was one row per launch, each of the earlier ones permanently down.
  const surviving = await embedded(page);
  expect(surviving).toHaveLength(1);
  expect(surviving[0].id).toBe(first.id);
  expect(surviving[0].instanceId).toBe(instanceId);

  // Stated as a count of the whole store as well, because that is the shape the
  // bug had: the bootstrap host plus one row per launch. Three launches meant
  // four rows, two of them permanently unreachable.
  //
  // One row now, not two: issue #613 stopped the desktop writing a same-origin
  // bootstrap host and forgets any it finds, so the row this used to count
  // alongside the embedded host no longer exists on a desktop launch. The
  // property under test is unchanged — three launches leave one row — and the
  // count is simply one smaller because the other row is gone for its own
  // reason.
  expect(await profiles(page), "one embedded host and nothing else").toHaveLength(1);

  // And the row that survived is the working one: this status comes back from a
  // real request to the real host, through the console's own probe.
  await openHostMenu(page);
  await expect(page.getByTestId(`host-row-${first.id}`)).toHaveAttribute("data-status", "live", {
    timeout: 30_000,
  });
});

test("the rows an older version left behind collapse on the next launch", async ({
  page,
  request,
  baseURL,
}) => {
  const instanceId = await instanceIdOf(request);
  const live = (baseURL ?? "").replace(/\/$/, "");

  // The state from the issue, verbatim: the bootstrap host, plus one dead
  // "This computer" per previous launch. None carries an identity, because no
  // version that wrote them reported one.
  await installDesktopShell(page, {
    addresses: [live],
    instanceId,
    seed: [
      { id: "5pnbp7zfx7w6", baseUrl: "", label: "This host" },
      { id: "vad0klxipf59", baseUrl: CLOSED_PORTS[0], label: "This computer" },
      { id: "4g4392soz5vm", baseUrl: CLOSED_PORTS[1], label: "This computer" },
    ].map((p) => ({ ...p, defaultCompany: null, credential: { kind: "cookie" } })),
  });

  await page.goto("/#/ledgers/tasks");
  await expect.poll(async () => (await embedded(page)).length, { timeout: 30_000 }).toBe(1);

  const [adopted] = await embedded(page);
  // Adopted rather than discarded: they were all this machine's host, so the
  // state scoped to one of them is this host's state.
  expect(["vad0klxipf59", "4g4392soz5vm"]).toContain(adopted.id);
  expect(adopted.baseUrl).toBe(live);
  expect(adopted.instanceId).toBe(instanceId);

  // The bootstrap connection is not this application's host, so adoption leaves
  // it alone — and #613, which this test's original form deferred to by name,
  // then forgets it: an unreachable same-origin row is dropped on restore
  // rather than carried forever. So the launch ends with the adopted host and
  // nothing else. Both halves are asserted here rather than only the count,
  // because "it was adopted" and "the dead row went" are different claims and
  // one row could satisfy a bare length check either way.
  const all = await profiles(page);
  expect(all.map((p) => p.id)).toEqual([adopted.id]);
});
