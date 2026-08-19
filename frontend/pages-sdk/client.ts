// The postMessage bridge to the parent console tab (docs/spec/runtime/pages.md
// §6, plan §6). A page never holds a credential of its own — every read or
// write it wants to run against the company's data goes through this bridge
// to the parent frame, which executes it with the operator's own
// authenticated session and posts the result back. Both queries and
// mutations travel the same way: GraphQL's own operation type is what
// distinguishes them, not this client.

const TIMEOUT_MS = 15_000;

// The per-document bridge capability handed to us by the console on load
// (`PagesView.tsx` mints a fresh one for every iframe document). Every
// `oc:graphql` message carries it, so the console can tell this exact
// document apart from one the page navigated itself to. Opaque-origin frames
// cannot share real storage or identity, so a document that replaces us has no
// way to learn this value.
let capability: string | null = null;
let capabilityWaiters: Array<(cap: string) => void> = [];

function waitForCapability(): Promise<string> {
  if (capability) return Promise.resolve(capability);
  return new Promise((resolve) => {
    capabilityWaiters.push(resolve);
  });
}

window.addEventListener("message", function onInit(event: MessageEvent) {
  const data = event.data as { type?: unknown; capability?: unknown } | null;
  if (data && data.type === "oc:init" && typeof data.capability === "string") {
    capability = data.capability;
    const waiters = capabilityWaiters;
    capabilityWaiters = [];
    for (const resolve of waiters) resolve(capability);
  }
});

/** The shape a GraphQL round trip resolves to, mirroring the server's own envelope. */
export interface GraphQLResult<T = unknown> {
  data?: T;
  errors?: unknown;
}

interface BridgeResultMessage {
  type: "oc:graphql:result";
  id: string;
  data?: unknown;
  errors?: unknown;
}

function isBridgeResult(value: unknown): value is BridgeResultMessage {
  return (
    typeof value === "object" &&
    value !== null &&
    (value as { type?: unknown }).type === "oc:graphql:result" &&
    typeof (value as { id?: unknown }).id === "string"
  );
}

/**
 * Runs one GraphQL operation — query or mutation — against the console's own
 * GraphQL endpoint, by way of the parent frame.
 *
 * Internally: generates a random correlation `id`, posts
 * `{type: "oc:graphql", id, capability, query, variables}` to `window.parent`, and
 * resolves when a matching `{type: "oc:graphql:result", id, ...}` reply
 * arrives — a one-shot listener that removes itself either way. The `id` is
 * what lets several concurrent calls share the same `window` without their
 * replies crossing.
 *
 * `targetOrigin` is deliberately `"*"` on the outgoing post, not this
 * document's real parent origin — because this document cannot know it. It
 * runs inside a `sandbox="allow-scripts"` iframe with no `allow-same-origin`
 * (docs/spec/runtime/pages.md §5), so its own origin is the opaque string
 * `"null"` and there is no legitimate origin to address the parent by. That
 * is intentional, not a shortcut: the real trust boundary is enforced on the
 * *parent* side (`PagesView.tsx`'s message listener), which only ever acts
 * on an event whose `source` is this exact iframe's own `contentWindow` —
 * something a page itself has no way to spoof.
 */
function query<T = unknown>(
  document: string,
  variables?: Record<string, unknown>,
): Promise<GraphQLResult<T>> {
  return new Promise((resolve, reject) => {
    const id =
      typeof crypto !== "undefined" && "randomUUID" in crypto
        ? crypto.randomUUID()
        : `${Date.now()}-${Math.random().toString(36).slice(2)}`;

    const timeout = window.setTimeout(() => {
      window.removeEventListener("message", onMessage);
      reject(new Error("oc:graphql timed out waiting for a reply from the console"));
    }, TIMEOUT_MS);

    function onMessage(event: MessageEvent) {
      if (!isBridgeResult(event.data) || event.data.id !== id) return;
      window.clearTimeout(timeout);
      window.removeEventListener("message", onMessage);
      resolve({ data: event.data.data as T | undefined, errors: event.data.errors });
    }

    window.addEventListener("message", onMessage);
    waitForCapability().then((cap) => {
      window.parent.postMessage(
        { type: "oc:graphql", id, capability: cap, query: document, variables },
        "*",
      );
    });
  });
}

/** The one live-data surface a page has: `client.query(document, variables)`. */
export const client = { query };
