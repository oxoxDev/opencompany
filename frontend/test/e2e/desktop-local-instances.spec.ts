import { expect, test, type Page } from "@playwright/test";

import { openHostMenu } from "./host-switcher";

/**
 * More than one host on this computer, driven through the whole app.
 *
 * The unit tests drive `adoptLocalHosts` directly. This drives the console the
 * way the packaged shell does — real browser, real `localStorage`, the built
 * bundle, a live host behind one of the instances — because the thing that
 * broke before (issue #615, and the prune this feature had to widen) lives in
 * `App`'s wiring rather than in the registry's exports.
 *
 * ## What is shimmed
 *
 * `window.__TAURI__` exists only inside the packaged shell, so the bridge is
 * shimmed and nothing else. It answers `oc_local_instances` from a roster the
 * spec holds, and `oc_create_local_instance` / `oc_start_local_instance` /
 * `oc_stop_local_instance` mutate that roster — which is exactly what
 * `LocalHosts` does in Rust, minus the sockets. Every decision asserted on
 * belongs to the console.
 *
 * Only one live host exists here, so the extra instances are given closed
 * ports. That is deliberate rather than a shortcut: a connection whose host is
 * unreachable must still get a row, and asserting on rows that cannot answer
 * proves the rail renders the *roster*, not just whatever replied.
 */

interface Instance {
  id: string;
  label: string;
  dataDir: string;
  running: boolean;
  baseUrl?: string;
  instanceId?: string;
  companies?: string[];
}

/** Installs a bridge backed by a roster the page can mutate, as the core is. */
async function installDesktopShell(page: Page, roster: Instance[]): Promise<void> {
  await page.addInitScript((seed: Instance[]) => {
    // The tour modal covers the console and swallows clicks.
    for (const key of ["oc-tour:single", "oc-tour:e2e-harness-co", "oc-tour:null"]) {
      window.localStorage.setItem(key, JSON.stringify({ skipped: true, seenAt: Date.now() }));
    }

    const instances: Instance[] = JSON.parse(JSON.stringify(seed)) as Instance[];
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
      core: {
        invoke(command: string, args: Record<string, string> = {}) {
          const find = (id: string) => instances.find((instance) => instance.id === id);
          switch (command) {
            case "oc_local_instances":
              return Promise.resolve(JSON.parse(JSON.stringify(instances)) as Instance[]);
            case "oc_create_local_instance": {
              // A fresh root, a fresh identity, a port of its own — the core's
              // shape, with a port nothing answers on.
              const id = args.label.toLowerCase().replace(/[^a-z0-9]+/g, "-");
              const created: Instance = {
                id,
                label: args.label,
                dataDir: `/tmp/instances/${id}`,
                running: true,
                baseUrl: "http://127.0.0.1:65999",
                instanceId: `instance-${id}`,
                companies: [],
              };
              instances.push(created);
              return Promise.resolve(created);
            }
            case "oc_start_local_instance": {
              const instance = find(args.id);
              if (!instance) return Promise.reject(new Error(`no such instance: ${args.id}`));
              instance.running = true;
              instance.baseUrl ??= "http://127.0.0.1:65998";
              instance.instanceId ??= `instance-${instance.id}`;
              return Promise.resolve(instance);
            }
            case "oc_stop_local_instance": {
              const instance = find(args.id);
              if (!instance) return Promise.reject(new Error(`no such instance: ${args.id}`));
              instance.running = false;
              return Promise.resolve(instance);
            }
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
  }, roster);
}

/** The roster every one of these starts from: one live host, one stopped. */
function seed(liveBaseUrl: string): Instance[] {
  return [
    {
      id: "default",
      label: "This computer",
      dataDir: "/tmp/e2e-embedded",
      running: true,
      baseUrl: liveBaseUrl,
      instanceId: "instance-default",
      companies: [],
    },
    {
      id: "scratch",
      label: "Scratch",
      dataDir: "/tmp/instances/scratch",
      running: false,
    },
  ];
}

/**
 * How many hosts the console holds, read off the switcher's closed trigger.
 *
 * The count has to be answerable while the roster dialog is open, which is when
 * every one of these asserts — and the menu the rows live in is shut by then.
 * `data-host-count` is on the trigger for exactly this: the roster size is not
 * a fact that should require opening a menu, which is the same reason the
 * trigger also carries the worst status across hosts. (It also sidesteps
 * `getByRole` skipping the `aria-hidden` subtree behind an open dialog.)
 */
function hostCount(page: Page) {
  return page.getByTestId("host-switcher");
}

async function openTheRoster(page: Page): Promise<void> {
  await openHostMenu(page);
  await page.getByTestId("host-switcher-add").click();
  await page.getByTestId("add-host-local").click();
  await expect(page.getByTestId("local-instances")).toBeVisible();
}

test("a stopped instance is listed and startable, not a broken row", async ({
  page,
  baseURL,
}) => {
  await installDesktopShell(page, seed(baseURL ?? ""));
  await page.goto("/");

  // One connection, because one instance is listening. A stopped instance has
  // no address, so a row for it in the menu could only fail its probe forever.
  await expect(hostCount(page)).toHaveAttribute("data-host-count", "1");

  await openTheRoster(page);
  const scratch = page.getByTestId("local-instance-scratch");
  await expect(scratch).toHaveAttribute("data-running", "false");
  await scratch.getByRole("button", { name: "Start" }).click();

  // Started, and now a host the console holds alongside the first.
  await expect(scratch).toHaveAttribute("data-running", "true");
  await expect(hostCount(page)).toHaveAttribute("data-host-count", "2");
});

test("a host started here can be stopped again without losing the others", async ({
  page,
  baseURL,
}) => {
  await installDesktopShell(page, seed(baseURL ?? ""));
  await page.goto("/");
  await openTheRoster(page);

  const scratch = page.getByTestId("local-instance-scratch");
  await scratch.getByRole("button", { name: "Start" }).click();
  await expect(hostCount(page)).toHaveAttribute("data-host-count", "2");

  await scratch.getByRole("button", { name: "Stop" }).click();
  await expect(scratch).toHaveAttribute("data-running", "false");
  // Back to the live host alone — and crucially *not* to zero: stopping one
  // instance must not prune the rows of the ones still running, which is the
  // failure the single-host prune would have produced.
  await expect(hostCount(page)).toHaveAttribute("data-host-count", "1");
  await expect(page.getByTestId("local-instance-default")).toHaveAttribute(
    "data-running",
    "true",
  );
});

test("a second company can be created on this computer", async ({ page, baseURL }) => {
  await installDesktopShell(page, seed(baseURL ?? ""));
  await page.goto("/");
  await openTheRoster(page);

  await page.getByLabel("Name").fill("Acme");
  await page.getByTestId("local-instance-add").click();

  await expect(page.getByTestId("local-instance-acme")).toHaveAttribute(
    "data-running",
    "true",
  );
  // Three instances now, two of them listening: the new one and the original.
  await expect(hostCount(page)).toHaveAttribute("data-host-count", "2");
});
