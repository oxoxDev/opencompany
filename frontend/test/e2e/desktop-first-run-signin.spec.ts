import { expect, test, type Page } from "@playwright/test";

/**
 * Issue #632 — the sign-in form a desktop operator can actually complete.
 *
 * The host half of that bug (an embedded host with no company, and no way to
 * create one) is fixed and proven in Rust, where a fresh data root is seeded and
 * signed into over HTTP. This is the console half: a packaged install admits one
 * standing local address, mails nothing, and answers every other address with
 * the same silent `202` — so a blank field asked a person to guess a credential
 * they have never seen, and guessing wrong looked exactly like guessing right.
 *
 * ## The launch this reproduces is the second one
 *
 * On a first launch the console has nothing remembered, so it waits for
 * `oc_embedded` before it renders a console at all and the suggestion is there
 * from the first paint. Every launch after that restores the embedded
 * connection from its profile *immediately* — the address and the operator
 * arrive over IPC a moment later — so the sign-in view mounts before either.
 * Seeding a profile and delaying the bridge is what puts this spec on the
 * ordinary path rather than the lucky one.
 *
 * The stub stands in for the packaged shell exactly as the sibling desktop
 * specs' does: `isDesktopRuntime()` is `"__TAURI__" in window` and nothing else,
 * and `oc_request` forwards to `fetch`, which is what the Rust proxy does. The
 * host, the console bundle and the boot sequence are all real.
 */

/** Signed out on purpose: the suite's shared session would skip the view. */
test.use({ storageState: { cookies: [], origins: [] } });

async function asDesktop(
  page: Page,
  config: { embedded: string; operatorEmail: string; discoveryDelayMs: number },
) {
  await page.addInitScript(
    (cfg: {
      embedded: string;
      operatorEmail: string;
      discoveryDelayMs: number;
    }) => {
      // The row a previous launch left behind, at the address this launch will
      // also serve. Restored synchronously, which is the point.
      window.localStorage.setItem(
        "oc.connections.v1",
        JSON.stringify([
          {
            id: "conn-embedded-remembered",
            baseUrl: cfg.embedded,
            label: "This computer",
            defaultCompany: null,
            credential: { kind: "cookie" },
            origin: "embedded",
          },
        ]),
      );

      const hosts = new Map<string, string>();
      // `withGlobalTauri` assigns the whole `@tauri-apps/api` bundle, and
      // `invoke`/`Channel` live under `core` — the v2 shape `tauriCore()` reads.
      // A shim that puts them at the top level is the v1 shape, which the bridge
      // rejects, leaving every proxied request `no such connection`.
      (window as unknown as { __TAURI__: unknown }).__TAURI__ = {
        core: {
          Channel: class {
            onmessage: ((message: string) => void) | null = null;
          },
          async invoke(
            command: string,
            args: Record<string, unknown>,
          ): Promise<unknown> {
            switch (command) {
              case "oc_connect":
                hosts.set(args.connectionId as string, args.baseUrl as string);
                return undefined;
              case "oc_disconnect":
                hosts.delete(args.connectionId as string);
                return undefined;
              case "oc_embedded":
                await new Promise((resolve) =>
                  setTimeout(resolve, cfg.discoveryDelayMs),
                );
                return {
                  baseUrl: cfg.embedded,
                  dataDir: "/tmp/e2e-desktop",
                  instanceId: "e2e-embedded-instance",
                  operatorEmail: cfg.operatorEmail,
                };
              case "oc_request": {
                const base = hosts.get(args.connectionId as string);
                if (base === undefined) {
                  throw new Error(
                    `no such connection: ${args.connectionId as string}`,
                  );
                }
                const req = args.request as {
                  method: string;
                  path: string;
                  headers: Record<string, string>;
                  body?: string;
                };
                const response = await fetch(base + req.path, {
                  method: req.method,
                  headers: req.headers,
                  body: req.body ?? undefined,
                  credentials: "include",
                });
                const text = await response.text();
                const headers: Record<string, string> = {};
                response.headers.forEach((value, name) => {
                  headers[name.toLowerCase()] = value;
                });
                return {
                  status: response.status,
                  statusText: response.statusText,
                  url: response.url,
                  text,
                  headers,
                };
              }
              default:
                return null;
            }
          },
        },
      };
    },
    config,
  );
}

test("a signed-out desktop offers the operator its own host admits", async ({
  page,
  baseURL,
}) => {
  const embedded = new URL(baseURL ?? "http://127.0.0.1:8080").origin;
  await asDesktop(page, {
    embedded,
    operatorEmail: "operator@opencompany.local",
    // Long enough that the remembered row is on screen first, which is what a
    // relaunch does anyway — see the note above.
    discoveryDelayMs: 750,
  });
  await page.goto("/#/ledgers/tasks");

  const email = page.getByLabel("Email");
  await expect(email).toBeVisible({ timeout: 30_000 });
  // THE assertion: the address arrives after the form did, and lands in it.
  await expect(email).toHaveValue("operator@opencompany.local", {
    timeout: 30_000,
  });
  await expect(page.getByTestId("suggested-email-hint")).toBeVisible();

  // A suggestion, not a lock. Somebody signing in as a teammate they invited to
  // their own host must be able to type over it, and the hint must go with it.
  await email.fill("someone@else.test");
  await expect(email).toHaveValue("someone@else.test");
  await expect(page.getByTestId("suggested-email-hint")).toHaveCount(0);
});
