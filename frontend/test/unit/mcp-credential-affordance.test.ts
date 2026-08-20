import { describe, expect, it } from "vitest";

import { credentialAffordance } from "@/views/connections/McpServersSection";

/**
 * Issue #1260. The MCP row offered a **Sign in** button on a server whose OAuth
 * could never complete: Slack's MCP endpoint answers `401` with a proper
 * resource-metadata challenge but advertises no RFC 7591 dynamic client
 * registration, so there is no client to mint. `POST …/oauth/start` refused
 * with a `400` naming the real remedy — paste a static token — which the
 * operator could only read by pressing a button that could not work.
 *
 * The host now distinguishes the two states. These pin the console half: which
 * control a row offers is a function of that hint and nothing else.
 */
describe("credentialAffordance", () => {
  it("offers sign-in only when the host says sign-in can complete", () => {
    expect(credentialAffordance("oauth_required")).toBe("sign_in");
  });

  it("offers a token field when OAuth is required but undrivable", () => {
    expect(credentialAffordance("static_token_required")).toBe("add_token");
  });

  it("never offers sign-in for a server that cannot complete one", () => {
    // The regression itself. Both codes carry `status: needs_config` and both
    // read as "an auth problem"; only this distinction stops the unusable
    // button coming back.
    expect(credentialAffordance("static_token_required")).not.toBe("sign_in");
  });

  it("routes a plain credential prompt to the same field", () => {
    expect(credentialAffordance("credential_required")).toBe("add_token");
  });

  it("offers nothing for a healthy server or an unknown code", () => {
    // A server that probed `ok` carries no hint at all, and must not sprout a
    // credential control; an unrecognised future code must not either, because
    // guessing which control it wants is how the wrong one gets offered.
    expect(credentialAffordance(undefined)).toBe("none");
    expect(credentialAffordance("token_rejected")).toBe("none");
    expect(credentialAffordance("some_code_added_later")).toBe("none");
  });

  it("leaves every List A provenance on the hint alone", () => {
    // Issue #1270 added a second parameter. The rule for a row that has a List
    // A declaration is unchanged — it is still a function of the hint and
    // nothing else, whether or not the caller passes the row.
    for (const source of ["manifest", "runtime", "default"] as const) {
      expect(credentialAffordance("oauth_required", { source, status: "needs_config" })).toBe(
        "sign_in",
      );
      expect(credentialAffordance("static_token_required", { source, status: "needs_config" })).toBe(
        "add_token",
      );
      expect(credentialAffordance(undefined, { source, status: "ok" })).toBe("none");
    }
  });
});

/**
 * Issue #1270. A directory install keeps its credentials as *named env values*
 * in the host's registry store, and its row's `name` is a display slug that
 * addresses no List A declaration. So both of List A's controls are wrong for
 * it twice over: `POST …/oauth/start` and `PUT …/mcp/servers/{name}` answer
 * `no MCP server named …`, and the value they collect would be written to a
 * store this server is not dialled from.
 */
describe("credentialAffordance — directory installs", () => {
  it("routes a registry row to its own env rotation", () => {
    expect(credentialAffordance(undefined, { source: "registry", status: "needs_config" })).toBe(
      "rotate_env",
    );
  });

  it("does not need a hint to know a registry row wants a credential", () => {
    // The reason `status` is consulted at all. The host's registry projection
    // emits a stable `authHint` only when the upstream connection reported one,
    // so an install refused for want of a credential can arrive as
    // `needs_config` with no hint — and a rule keyed on the hint alone would
    // offer that row nothing at all.
    expect(credentialAffordance(undefined, { source: "registry", status: "needs_config" })).not.toBe(
      "none",
    );
  });

  it("never offers a registry row a control that writes to the wrong store", () => {
    for (const hint of [
      undefined,
      "oauth_required",
      "static_token_required",
      "credential_required",
      "token_rejected",
    ]) {
      const offered = credentialAffordance(hint, {
        source: "registry",
        status: "needs_config",
      });
      expect(offered).not.toBe("sign_in");
      expect(offered).not.toBe("add_token");
    }
  });

  it("offers nothing to a registry row that is not asking for one", () => {
    for (const status of ["ok", "error", "unknown"] as const) {
      expect(credentialAffordance(undefined, { source: "registry", status })).toBe("none");
    }
    expect(credentialAffordance(undefined, { source: "registry" })).toBe("none");
  });
});
