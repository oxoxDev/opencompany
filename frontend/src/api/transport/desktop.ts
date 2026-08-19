// Telling the Rust core which hosts this console talks to.
//
// `ProxyTransport` addresses a host by connection id, and the core resolves
// that id against `ProxyRegistry`. Nothing in the console registered anything,
// so every proxied request came back `no such connection: <id>` — the desktop
// could not complete one round trip. This module is the missing half.
//
// ## Registration is awaited, not fired and forgotten
//
// `addConnection` is synchronous (React renders off it) and `oc_connect` is
// not. Kicking the command off and hoping it lands first is a race the console
// loses on a fast probe, and the symptom — an unreachable host that becomes
// reachable on retry — reads like a network fault rather than an ordering bug.
//
// So each registration is kept as a promise and `ProxyTransport` awaits it
// before its first call. After that the promise is already resolved and the
// await costs a microtask.
//
// ## No implicit current connection
//
// Every entry point here takes an explicit id, for the same reason the Rust
// side does: a single "active connection" is what stops comparable clients
// from holding more than one host, and a convenience default here would
// reintroduce it above the seam instead of below it.

import { tauriCore } from "./bridge";

/** What `oc_embedded` answers with. Mirrors `EmbeddedInfo` in Rust. */
export interface EmbeddedInfo {
  baseUrl: string;
  dataDir: string;
  /**
   * Who is listening at `baseUrl`, as opposed to where.
   *
   * Optional here though the core always sends it, because this is the one
   * field a *stale* build of the shell would omit: the console is bundled into
   * the binary, but a developer running `pnpm dev` against an older `cargo`
   * build is an ordinary Tuesday. Absent degrades to the pre-identity
   * behaviour rather than to a connection keyed on `undefined`.
   */
  instanceId?: string;
  /**
   * The address this host will sign a person in as (#632).
   *
   * A desktop install has one standing admin and no mail transport, so nobody
   * could guess what to type and every other address gets the same silent
   * acknowledgement. Optional for the same reason as `instanceId`: an older
   * shell does not send it, and a blank sign-in form is the honest degrade.
   */
  operatorEmail?: string;
}

/**
 * How a connection authenticates, in the shape `oc_connect` takes.
 *
 * Note what is absent: any device token. The core resolves a paired device's
 * session from the keychain by connection id, so the console has nothing to
 * pass and — more to the point — nothing to leak. Only the platform bearer
 * travels, because that one genuinely arrives in the URL.
 */
export interface DesktopCredential {
  platformToken?: string;
}

/** What pairing tells the console. Carries no secret. */
export interface PairedDevice {
  company: string;
  deviceId: string;
  expiresAtMillis: number;
}

/** Connections the core has been told about, by id. */
const registrations = new Map<string, Promise<void>>();

/**
 * Registers a host with the core, replacing any previous registration for `id`.
 *
 * Resolves when the core has the connection. A failure is swallowed into a
 * resolved promise rather than left rejected: the transport awaits this on
 * every call, and an unhandled rejection stored in a module-level map would
 * resurface on each one. The request that follows fails on its own merits with
 * `no such connection`, which is the honest error and the one the console
 * already renders per row.
 */
export function registerConnection(
  id: string,
  baseUrl: string,
  credential: DesktopCredential = {},
): Promise<void> {
  const desktop = tauriCore();
  if (!desktop) return Promise.resolve();

  // Chained onto whatever is already parked under this id, in both directions.
  // `forgetConnection` sequences its disconnect behind a pending registration;
  // without the same courtesy here, a re-register racing an in-flight
  // `oc_disconnect` can land first and then be disconnected by it — the same
  // ordering bug, mirrored. Whichever call came last wins, which is what a
  // caller means by calling it.
  const previous = registrations.get(id) ?? Promise.resolve();
  const pending = previous
    .then(() =>
      desktop.invoke<void>("oc_connect", {
        connectionId: id,
        baseUrl,
        platformToken: credential.platformToken ?? null,
      }),
    )
    .then(
      () => undefined,
      (error: unknown) => {
        console.error(`[desktop] could not register connection ${id}`, error);
      },
    );
  registrations.set(id, pending);
  return pending;
}

/**
 * Drops a host from the core.
 *
 * Sequenced after any registration still in flight. This and
 * `registerConnection` race the same way a request does: `oc_connect` resolving
 * *after* `oc_disconnect` landed would leave the connection registered in the
 * core while the console believes it is gone — a dangling entry that the next
 * `registerConnection` for a reused id may or may not overwrite cleanly.
 *
 * Stays synchronous because callers are React event handlers, so the ordering
 * is expressed by chaining onto the pending promise rather than by awaiting it.
 * The map entry is cleared only once the disconnect has been issued; deleting
 * it up front is what dropped the ordering in the first place.
 */
export function forgetConnection(id: string): void {
  const desktop = tauriCore();
  if (!desktop) {
    registrations.delete(id);
    return;
  }
  const pending = registrations.get(id) ?? Promise.resolve();
  const disconnected: Promise<void> = pending
    .then(() => desktop.invoke<void>("oc_disconnect", { connectionId: id }))
    .then(
      () => undefined,
      (error: unknown) => {
        console.error(`[desktop] could not drop connection ${id}`, error);
      },
    )
    .finally(() => {
      // Only remove the entry this call created. A `registerConnection` that
      // landed while the disconnect was in flight owns the id now, and clearing
      // its promise would let a request run before its registration.
      if (registrations.get(id) === disconnected) registrations.delete(id);
    });
  // Parked under the same id so a `connectionReady` in between waits for the
  // disconnect rather than racing past it.
  registrations.set(id, disconnected);
}

/**
 * Resolves once `id` is registered, immediately when there is nothing pending.
 *
 * The "nothing pending" case is not an error: it covers the browser build and
 * any caller that registered before this module was involved.
 */
export function connectionReady(id: string): Promise<void> {
  return registrations.get(id) ?? Promise.resolve();
}

/**
 * The in-process host, when this build has one.
 *
 * `null` in a browser, and also in a desktop whose embedded host failed to
 * start — most often because another instance holds the data root. That is a
 * state the console shows rather than a reason to have no console: the point of
 * the desktop is that it can talk to remote hosts too.
 */
export async function embeddedHost(): Promise<EmbeddedInfo | null> {
  const desktop = tauriCore();
  if (!desktop) return null;
  try {
    return (await desktop.invoke<EmbeddedInfo | null>("oc_embedded")) ?? null;
  } catch (error) {
    console.error("[desktop] could not read the embedded host", error);
    return null;
  }
}

/** One host this machine runs. Mirrors `LocalInstanceInfo` in Rust. */
export interface LocalInstance {
  /** Stable within this machine, and the name of its data directory. */
  id: string;
  /** What the operator called it. Free text, and renameable. */
  label: string;
  dataDir: string;
  running: boolean;
  /** Present exactly when `running` — a stopped instance has no port. */
  baseUrl?: string;
  /**
   * The host's own durable identity, which is what a connection row is keyed
   * on. The address is not: it is a fresh ephemeral port every launch.
   */
  instanceId?: string;
  operatorEmail?: string;
  companies?: string[];
  /** Why it is not running. Usually another process holding its data root. */
  error?: string;
}

/**
 * Every host this machine runs, listening or not.
 *
 * `[]` in a browser, which runs none. Also `[]` on a shell built before the
 * roster existed: the command is simply absent there, and `App` falls back to
 * {@link embeddedHost} — one instance is what that shell has anyway, so the
 * degrade is exact rather than approximate.
 */
export async function localInstances(): Promise<LocalInstance[] | null> {
  const desktop = tauriCore();
  if (!desktop) return [];
  try {
    const answer = await desktop.invoke<LocalInstance[]>("oc_local_instances");
    // Anything that is not an array is a shell that does not implement this.
    // Checked rather than defaulted to `[]`: an unknown command answers
    // `undefined` on some bridges and rejects on others, and both mean the same
    // thing — ask `oc_embedded` instead.
    return Array.isArray(answer) ? answer : null;
  } catch (error) {
    // `null`, not `[]`: "this shell has no roster command" and "this machine
    // runs nothing" are different answers, and only the first has a fallback.
    console.warn("[desktop] this shell has no instance roster", error);
    return null;
  }
}

/**
 * Adds a host on this machine over a data root of its own, and starts it.
 *
 * A root of its own is the whole mechanism: two hosts over one root overwrite
 * each other's companies, which is why the core locks it. So a second local
 * company is a second root, not a second process.
 */
export async function createLocalInstance(label: string): Promise<LocalInstance> {
  const desktop = tauriCore();
  if (!desktop) throw new Error("running a host locally needs the desktop application");
  return desktop.invoke<LocalInstance>("oc_create_local_instance", { label });
}

export async function startLocalInstance(id: string): Promise<LocalInstance> {
  const desktop = tauriCore();
  if (!desktop) throw new Error("running a host locally needs the desktop application");
  return desktop.invoke<LocalInstance>("oc_start_local_instance", { id });
}

/**
 * Stops a host, freeing its port and its data root.
 *
 * Freeing the root is the part worth wanting: it is what lets an
 * `opencompany serve` in a terminal take over the same company.
 */
export async function stopLocalInstance(id: string): Promise<LocalInstance> {
  const desktop = tauriCore();
  if (!desktop) throw new Error("running a host locally needs the desktop application");
  return desktop.invoke<LocalInstance>("oc_stop_local_instance", { id });
}

export async function renameLocalInstance(id: string, label: string): Promise<LocalInstance> {
  const desktop = tauriCore();
  if (!desktop) throw new Error("running a host locally needs the desktop application");
  return desktop.invoke<LocalInstance>("oc_rename_local_instance", { id, label });
}

/**
 * Drops a host from the roster. **The data stays on disk** — the core does the
 * reversible half only, because the other half is someone's company.
 */
export async function forgetLocalInstance(id: string): Promise<void> {
  const desktop = tauriCore();
  if (!desktop) throw new Error("running a host locally needs the desktop application");
  await desktop.invoke<void>("oc_forget_local_instance", { id });
}

/**
 * Redeems a pairing code for this machine.
 *
 * The token never comes back. It exists for one HTTP response, and the core
 * keeps that response to itself: it writes the session to the OS keychain and
 * answers with only what a person needs to see. A console that received the
 * token — even to hand it straight back — would be a console an injected script
 * could read it from.
 */
export async function pairDevice(
  id: string,
  baseUrl: string,
  code: string,
  label?: string,
): Promise<PairedDevice> {
  const desktop = tauriCore();
  if (!desktop) throw new Error("pairing a device needs the desktop application");
  return desktop.invoke<PairedDevice>("oc_pair_device", {
    connectionId: id,
    baseUrl,
    code,
    label: label ?? null,
  });
}

/**
 * Forgets this machine's stored session for a connection.
 *
 * Local only: the session still exists on the host until someone revokes it
 * from the devices list there. Conflating the two would mean unpairing one
 * laptop cut off another.
 */
export async function forgetDevice(id: string): Promise<void> {
  await tauriCore()?.invoke("oc_forget_device", { connectionId: id });
}

/** Test seam: forget every registration. */
export function resetDesktopRegistrations(): void {
  registrations.clear();
}
