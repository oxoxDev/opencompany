import { describe, expect, it } from "vitest";

import { fieldCopy, fieldPlaceholder, hasFieldCopy } from "@/lib/setup-fields";

/**
 * The wizard's settings copy. `GET /api/v1/setup` sends only the dotted
 * `config.toml` key, so every human word on that screen is the console's, and
 * a key added host-side without copy here silently renders as a raw key again.
 */

/** Every key the wizard lists — the ADVANCED_GROUPS fields plus the Model step. */
const LISTED = [
  "auth_mode",
  "brain_mode",
  "api_url",
  "openhuman_url",
  "bind",
  "public_url",
  "workspace.max_blob_mb",
  "workspace.storage_quota_gb",
  "github_token",
  "tinyhumans_api_key",
];

describe("field copy", () => {
  /** The regression this file exists for: a settings screen that reads as a
   * `.toml` file with input boxes. */
  it("has real copy for every field the wizard shows", () => {
    const missing = LISTED.filter((key) => !hasFieldCopy(key));
    expect(missing, `no copy for: ${missing.join(", ")}`).toEqual([]);
  });

  it("never shows the raw key as the label", () => {
    for (const key of LISTED) {
      expect(fieldCopy(key).label).not.toBe(key);
    }
  });

  /** A host that grows a field before this map does must still render. */
  it("falls back to a readable form rather than nothing", () => {
    expect(fieldCopy("workspace.some_new_knob").label).toBe("Workspace some new knob");
  });
});

describe("placeholders", () => {
  const field = (over: Partial<Parameters<typeof fieldPlaceholder>[0]> = {}) => ({
    key: "bind",
    layer: "default",
    value: null,
    secret: false,
    ...over,
  });

  /** The value in force beats a description of the mechanism: "set by default"
   * told an operator nothing about the state they were in. */
  it("shows the value when there is one", () => {
    expect(fieldPlaceholder(field({ value: "127.0.0.1:8080" }))).toBe("127.0.0.1:8080");
  });

  it("names where an unset value comes from", () => {
    expect(fieldPlaceholder(field())).toBe("Using the default");
    expect(fieldPlaceholder(field({ layer: "manifest" }))).toContain("manifest");
    expect(fieldPlaceholder(field({ layer: "env" }))).toContain("environment");
  });

  it("carries the unit where one exists, so a bare number is unambiguous", () => {
    expect(fieldPlaceholder(field({ key: "workspace.max_blob_mb" }))).toContain("MB");
    expect(fieldPlaceholder(field({ key: "workspace.storage_quota_gb" }))).toContain("GB");
  });

  /** A secret's value is never sent, so the placeholder is the only thing that
   * can say whether one is already stored. */
  it("distinguishes a stored secret from an absent one", () => {
    expect(fieldPlaceholder(field({ secret: true, value: null }))).toBe("Not set");
    expect(fieldPlaceholder(field({ secret: true, value: "set" }))).toContain("Stored");
  });
});
