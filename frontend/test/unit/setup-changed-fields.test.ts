import { describe, expect, it } from "vitest";

import { changedFields, type SetupField, type SetupStatus } from "@/api/setup";

/**
 * The setup wizard's submit payload.
 *
 * Every rule here fails silently if it is wrong, which is why they are tested
 * away from the component: a wrongly-included env-owned field fails the whole
 * apply (it is all-or-nothing) over a box the form rendered read-only; a
 * wrongly-omitted `null` leaves a stale key in `config.toml`; and a wrongly
 * *included* empty secret would wipe a working credential with `""`.
 */
function field(over: Partial<SetupField> & { key: string }): SetupField {
  return {
    value: null,
    layer: "default",
    editable: true,
    requires_restart: false,
    secret: false,
    ...over,
  };
}

function status(...fields: SetupField[]): SetupStatus {
  return {
    complete: false,
    config_path: "/data/config.toml",
    fields,
    templates: [],
    auth_modes: ["email"],
    build: {
      acp_in_build: false,
      acp_transport_mounted: false,
      mcp_in_build: false,
      harness_in_build: false,
      oauth_in_build: false,
    },
    companies: [],
  inference: { ready: false, provider: null, base_url: null },
  };
}

describe("changedFields", () => {
  it("sends only what actually changed", () => {
    const s = status(
      field({ key: "bind", value: "127.0.0.1:8080" }),
      field({ key: "api_url", value: "https://api.example" }),
    );

    expect(
      changedFields(s, { bind: "0.0.0.0:9000", api_url: "https://api.example" }),
    ).toEqual({ bind: "0.0.0.0:9000" });
  });

  it("returns nothing when the form still holds the file's values", () => {
    const s = status(field({ key: "bind", value: "127.0.0.1:8080" }));
    expect(changedFields(s, { bind: "127.0.0.1:8080" })).toEqual({});
  });

  /**
   * `config.toml` sits *below* the environment in precedence, so a write to an
   * env-owned field would be saved and then ignored at the next boot. The host
   * refuses it; sending one anyway would fail the entire apply.
   */
  it("never sends a field the environment owns", () => {
    const s = status(
      field({ key: "auth_mode", value: "wallet", layer: "env", editable: false }),
    );
    expect(changedFields(s, { auth_mode: "email" })).toEqual({});
  });

  /**
   * Clearing a key lets the next precedence layer supply it. Writing `""`
   * instead would be a set-but-empty value that shadows that layer.
   */
  it("clears an emptied field with null rather than an empty string", () => {
    const s = status(field({ key: "public_url", value: "https://old.example" }));
    expect(changedFields(s, { public_url: "" })).toEqual({ public_url: null });
  });

  it("treats a field the operator never touched as unchanged, not cleared", () => {
    const s = status(field({ key: "public_url", value: "https://keep.example" }));
    // No entry in `values` at all — the step was skipped.
    expect(changedFields(s, {})).toEqual({});
  });

  /**
   * A secret's current value is never echoed by the host, so an empty box means
   * "leave it alone". Reading it as a clear would delete a working credential
   * every time an operator walked through setup without retyping it.
   */
  it("sends a secret only when one was typed", () => {
    const s = status(field({ key: "tinyhumans_api_key", secret: true, value: null }));

    expect(changedFields(s, {})).toEqual({});
    expect(changedFields(s, { tinyhumans_api_key: "" })).toEqual({});
    expect(changedFields(s, { tinyhumans_api_key: "sk-new" })).toEqual({
      tinyhumans_api_key: "sk-new",
    });
  });

  it("does not send a secret the environment owns", () => {
    const s = status(
      field({ key: "tinyhumans_api_key", secret: true, layer: "env", editable: false }),
    );
    expect(changedFields(s, { tinyhumans_api_key: "sk-new" })).toEqual({});
  });

  it("writes a value into a field the file does not set yet", () => {
    const s = status(field({ key: "workspace.max_blob_mb", value: null }));
    expect(changedFields(s, { "workspace.max_blob_mb": "64" })).toEqual({
      "workspace.max_blob_mb": "64",
    });
  });
});
