// @vitest-environment jsdom

import { beforeEach, describe, expect, it } from "vitest";

import {
  adoptEmbeddedHost,
  adoptLocalHosts,
  getConnection,
  listConnections,
  resetConnections,
  restoreConnections,
} from "@/connections/registry";
import {
  createLocalInstance,
  forgetLocalInstance,
  localInstances,
  startLocalInstance,
  stopLocalInstance,
} from "@/api/transport/desktop";
import { readProfiles } from "@/connections/profileStore";
import { scopedKey } from "@/connections/types";

/**
 * More than one host on one machine.
 *
 * The desktop used to run exactly one: a single data root, a single embedded
 * host, and — in the console — a prune that treated *any other* embedded
 * profile as a dead row from a previous launch. That prune is the part that
 * cannot survive a roster: an operator's second local company looks exactly
 * like last launch's ghost, because both are "an embedded profile that is not
 * the one being adopted".
 *
 * So these tests are about the set: every running instance keeps its own row
 * and its own id across relaunches, and only the instances that really are gone
 * are dropped.
 */

const ACME = "0f9d8c7b6a5e4f3d2c1b0a9988776655";
const BEAM = "1122334455667788aabbccddeeff0011";

beforeEach(() => {
  resetConnections();
  window.localStorage.clear();
});

function relaunch(): void {
  resetConnections();
  restoreConnections();
}

describe("several hosts on this machine", () => {
  it("keeps a row per instance", () => {
    const [acme, beam] = adoptLocalHosts([
      { baseUrl: "http://127.0.0.1:65145", instanceId: ACME, label: "This computer" },
      { baseUrl: "http://127.0.0.1:65146", instanceId: BEAM, label: "Acme" },
    ]);

    expect(acme).not.toBe(beam);
    expect(listConnections()).toHaveLength(2);
    expect(getConnection(beam)?.label).toBe("Acme");
  });

  it("gives each instance an id that survives a relaunch", () => {
    // THE regression the single-host prune would reintroduce. Both instances
    // move to a fresh ephemeral port on every launch, so neither is
    // recognisable by address — and every browser-local key is scoped by the
    // connection id, so a re-mint orphans one instance's tour state, last-read
    // channel and mail draft with nothing reporting it.
    const before = adoptLocalHosts([
      { baseUrl: "http://127.0.0.1:65145", instanceId: ACME },
      { baseUrl: "http://127.0.0.1:65146", instanceId: BEAM },
    ]);

    relaunch();
    const after = adoptLocalHosts([
      { baseUrl: "http://127.0.0.1:51001", instanceId: ACME },
      { baseUrl: "http://127.0.0.1:51002", instanceId: BEAM },
    ]);

    expect(after).toEqual(before);
    expect(listConnections()).toHaveLength(2);
    expect(readProfiles()).toHaveLength(2);
    expect(scopedKey("oc-tour", { connection: after[1], company: null })).toBe(
      scopedKey("oc-tour", { connection: before[1], company: null }),
    );
  });

  it("follows the port each instance is actually listening on", () => {
    adoptLocalHosts([
      { baseUrl: "http://127.0.0.1:65145", instanceId: ACME },
      { baseUrl: "http://127.0.0.1:65146", instanceId: BEAM },
    ]);
    relaunch();
    const [acme, beam] = adoptLocalHosts([
      { baseUrl: "http://127.0.0.1:51001", instanceId: ACME },
      { baseUrl: "http://127.0.0.1:51002", instanceId: BEAM },
    ]);

    expect(getConnection(acme)?.baseUrl).toBe("http://127.0.0.1:51001");
    expect(getConnection(beam)?.baseUrl).toBe("http://127.0.0.1:51002");
  });

  it("drops only the instances that are really gone", () => {
    const [, beam] = adoptLocalHosts([
      { baseUrl: "http://127.0.0.1:65145", instanceId: ACME },
      { baseUrl: "http://127.0.0.1:65146", instanceId: BEAM },
    ]);

    // Someone removed the Acme instance from the roster. The other one is not
    // a ghost — it is the company they are still using.
    relaunch();
    const [still] = adoptLocalHosts([
      { baseUrl: "http://127.0.0.1:51002", instanceId: BEAM },
    ]);

    expect(still).toBe(beam);
    expect(listConnections()).toHaveLength(1);
    expect(readProfiles()).toHaveLength(1);
  });

  it("keeps a stopped instance's id, so starting it again resumes its state", () => {
    // Stopping is not forgetting. `removeConnection` forgets the persisted
    // profile, and the connection id is what every browser-local key is scoped
    // by — so pruning a stopped instance as a ghost orphans its tour state,
    // last-read channel and mail draft, and it comes back wearing a new id.
    // That is #615 reached by pressing Stop instead of by relaunching.
    const [, beam] = adoptLocalHosts([
      { baseUrl: "http://127.0.0.1:65145", instanceId: ACME },
      { baseUrl: "http://127.0.0.1:65146", instanceId: BEAM },
    ]);
    const key = scopedKey("oc-tour", { connection: beam, company: null });

    // Beam is stopped: no longer a running host, but still on the roster.
    adoptLocalHosts([{ baseUrl: "http://127.0.0.1:65145", instanceId: ACME }], [ACME, BEAM]);

    expect(listConnections().map((c) => c.id)).not.toContain(beam);
    expect(readProfiles().map((p) => p.id)).toContain(beam);

    // And started again — on a fresh port, as every restart is.
    const [, restarted] = adoptLocalHosts(
      [
        { baseUrl: "http://127.0.0.1:65145", instanceId: ACME },
        { baseUrl: "http://127.0.0.1:51002", instanceId: BEAM },
      ],
      [ACME, BEAM],
    );

    expect(restarted).toBe(beam);
    expect(scopedKey("oc-tour", { connection: restarted, company: null })).toBe(key);
  });

  it("still forgets a host the core no longer has at all", () => {
    // The other half: retention must not become "never prune", or the dead
    // rows #615 is about accumulate again — now permanently, since nothing
    // would ever remove them.
    const [, beam] = adoptLocalHosts([
      { baseUrl: "http://127.0.0.1:65145", instanceId: ACME },
      { baseUrl: "http://127.0.0.1:65146", instanceId: BEAM },
    ]);

    // Beam is gone from the roster entirely — forgotten, not stopped.
    adoptLocalHosts([{ baseUrl: "http://127.0.0.1:65145", instanceId: ACME }], [ACME]);

    expect(listConnections().map((c) => c.id)).not.toContain(beam);
    expect(readProfiles().map((p) => p.id)).not.toContain(beam);
  });

  it("does not let two instances adopt one id-less profile", () => {
    // What an older shell wrote: one embedded row, no identity recorded,
    // because no version that wrote it reported one. Exactly one instance may
    // inherit it — two would share a connection id, and with it every scoped
    // key, which is the failure `types.ts` exists to prevent.
    window.localStorage.setItem(
      "oc.connections.v1",
      JSON.stringify([
        {
          id: "vad0klxipf59",
          baseUrl: "http://127.0.0.1:65275",
          label: "This computer",
          defaultCompany: null,
          credential: { kind: "cookie" },
        },
      ]),
    );
    restoreConnections();

    const [acme, beam] = adoptLocalHosts([
      { baseUrl: "http://127.0.0.1:51001", instanceId: ACME },
      { baseUrl: "http://127.0.0.1:51002", instanceId: BEAM },
    ]);

    expect(acme).toBe("vad0klxipf59");
    expect(beam).not.toBe(acme);
    expect(listConnections()).toHaveLength(2);
  });

  it("takes the name the core reports over the remembered one", () => {
    // Renaming happens in the core, which owns the roster. A label written to
    // `localStorage` at first sight would otherwise outrank it forever.
    const [id] = adoptLocalHosts([
      { baseUrl: "http://127.0.0.1:65145", instanceId: ACME, label: "Acme" },
    ]);
    relaunch();
    adoptLocalHosts([
      { baseUrl: "http://127.0.0.1:51001", instanceId: ACME, label: "Acme Holdings" },
    ]);

    expect(getConnection(id)?.label).toBe("Acme Holdings");
    expect(readProfiles()[0]?.label).toBe("Acme Holdings");
  });

  it("still behaves as one host for a shell that reports one", () => {
    // `adoptEmbeddedHost` is the one-host call, kept because the shell and the
    // console ship independently: a `pnpm dev` console against an older
    // `cargo` build gets exactly this path.
    const one = adoptEmbeddedHost({ baseUrl: "http://127.0.0.1:65145", instanceId: ACME });
    relaunch();
    const again = adoptEmbeddedHost({
      baseUrl: "http://127.0.0.1:51001",
      instanceId: ACME,
    });

    expect(again).toBe(one);
    expect(listConnections()).toHaveLength(1);
  });
});

/**
 * The IPC half: what the console asks the core, and how it degrades when the
 * core has never heard of the question.
 *
 * The shell and the console ship independently — the bundle is embedded in the
 * binary, but `pnpm dev` against an older `cargo build` is an ordinary Tuesday.
 * So "this shell has no roster" has to be a distinct answer from "this machine
 * runs nothing", or a developer's console silently shows an empty roster for a
 * desktop that is in fact serving their company.
 */
describe("asking the core what it runs", () => {
  interface Invocation {
    command: string;
    args: Record<string, unknown>;
  }

  let calls: Invocation[] = [];

  function installBridge(answers: Record<string, unknown | Error>): void {
    calls = [];
    (window as unknown as { __TAURI__: unknown }).__TAURI__ = {
      core: {
        invoke: (command: string, args: Record<string, unknown> = {}) => {
          calls.push({ command, args });
          const answer = answers[command];
          if (answer instanceof Error) return Promise.reject(answer);
          return Promise.resolve(answer ?? undefined);
        },
        Channel: class {
          onmessage: ((message: string) => void) | null = null;
        },
      },
    };
  }

  function uninstallBridge(): void {
    delete (window as unknown as { __TAURI__?: unknown }).__TAURI__;
  }

  it("answers with an empty roster in a browser", async () => {
    uninstallBridge();
    await expect(localInstances()).resolves.toEqual([]);
  });

  it("tells a missing command apart from an empty machine", async () => {
    // `null`, not `[]`. Only the first has a fallback, and conflating them is
    // what turns "your shell is old" into "you have no companies".
    installBridge({ oc_local_instances: new Error("Command oc_local_instances not found") });
    await expect(localInstances()).resolves.toBeNull();

    installBridge({ oc_local_instances: [] });
    await expect(localInstances()).resolves.toEqual([]);
  });

  it("passes each command the arguments the core names", async () => {
    // The core reads these by name and nothing type-checks one side against
    // the other: a renamed argument lands as a command that silently does
    // nothing, not as an error.
    installBridge({
      oc_create_local_instance: { id: "acme", label: "Acme", dataDir: "/d", running: true },
      oc_start_local_instance: { id: "acme", label: "Acme", dataDir: "/d", running: true },
      oc_stop_local_instance: { id: "acme", label: "Acme", dataDir: "/d", running: false },
      oc_forget_local_instance: undefined,
    });

    await createLocalInstance("Acme");
    await startLocalInstance("acme");
    await stopLocalInstance("acme");
    await forgetLocalInstance("acme");

    expect(calls).toEqual([
      { command: "oc_create_local_instance", args: { label: "Acme" } },
      { command: "oc_start_local_instance", args: { id: "acme" } },
      { command: "oc_stop_local_instance", args: { id: "acme" } },
      { command: "oc_forget_local_instance", args: { id: "acme" } },
    ]);
    uninstallBridge();
  });

  it("refuses to pretend a browser can run a host", async () => {
    // Not a silent no-op: a caller that thought it created a company and got
    // nothing is worse than one that sees why it could not.
    uninstallBridge();
    await expect(createLocalInstance("Acme")).rejects.toThrow(/desktop application/);
  });
});
