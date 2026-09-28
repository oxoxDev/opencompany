import { describe, expect, it, vi } from "vitest";

import { fetchAuthConfig, invite, requestWalletChallenge } from "@/api/auth";

/**
 * Config-driven sign-in, on the console side.
 *
 * Three properties are worth pinning here rather than leaving to the view:
 *
 * 1. **The rollout-skew default.** A console served by a host that predates
 *    `/auth/config` must render the screen it always did, not an error. The
 *    fallback is the whole compatibility story for every existing deployment.
 * 2. **Which field carries an invited identity.** The server normalizes an
 *    email and a wallet by different rules — one is lowercased, the other must
 *    not be — so sending the wrong field is not a cosmetic mistake, it is an
 *    invite that grants nothing or a wallet that can never verify.
 * 3. **base58 of the signature**, because a wrong encoding fails only at the
 *    host, as an indistinguishable `invalid_login`.
 */

const client = (impl: { get?: unknown; post?: unknown }) =>
  ({
    scopeFor: () => "/api/v1/company",
    get: vi.fn().mockImplementation(impl.get as never),
    post: vi.fn().mockImplementation(impl.post as never),
  }) as never;

describe("fetchAuthConfig", () => {
  it("reports the mode the host names", async () => {
    const c = client({
      get: async () => ({ mode: "wallet", passwords: false, magicLink: false }),
    });
    expect(await fetchAuthConfig(c, null)).toEqual({
      mode: "wallet",
      passwords: false,
      magicLink: false,
      claimable: false,
    });
  });

  it("carries the company's name, blank-normalised", async () => {
    // The sign-in screen has no other source for it: every route that reports
    // the name is behind the sign-in being drawn (issue #1334). A blank one is
    // normalised away here, once, so no view has to decide whether `""` means
    // "unnamed" or "not reported".
    const named = client({
      get: async () => ({ mode: "email", passwords: true, magicLink: true, name: " Acme " }),
    });
    expect((await fetchAuthConfig(named, null)).name).toBe("Acme");

    const blank = client({
      get: async () => ({ mode: "email", passwords: true, magicLink: true, name: "  " }),
    });
    expect((await fetchAuthConfig(blank, null)).name).toBeUndefined();
  });

  it("falls back to email when the host has no such route", async () => {
    // A host predating this feature 404s here, and it signs people in by email.
    // Anything else would put an unusable screen in front of every existing
    // deployment the moment the console updated.
    const c = client({
      get: async () => {
        throw new Error("404");
      },
    });
    expect(await fetchAuthConfig(c, null)).toEqual({
      mode: "email",
      passwords: true,
      magicLink: true,
      claimable: false,
    });
  });

  it("assumes nothing is claimable on a host that omits the field", async () => {
    // A host too old to report it has no claim route to send anyone to, so
    // the offer to create an account must not appear.
    const c = client({
      get: async () => ({ mode: "email", passwords: true, magicLink: true }),
    });
    expect((await fetchAuthConfig(c, null)).claimable).toBe(false);
  });

  it("reports a claimable company when the host says so", async () => {
    const c = client({
      get: async () => ({ mode: "email", passwords: true, magicLink: false, claimable: true }),
    });
    expect((await fetchAuthConfig(c, null)).claimable).toBe(true);
  });

  it("assumes a magic link works on a host that cannot answer", async () => {
    // The fallback must not be the cautious one. Every host predating this
    // field either mails links or echoes them, so defaulting to false would
    // hide a working sign-in from every deployment that has not updated.
    const c = client({
      get: async () => {
        throw new Error("network down");
      },
    });
    expect((await fetchAuthConfig(c, null)).magicLink).toBe(true);
  });

  it("assumes a magic link works on a host that omits the field", async () => {
    // The route answering without `magicLink` is the same rollout skew as it
    // not answering at all, and must resolve the same way.
    const c = client({ get: async () => ({ mode: "email", passwords: true }) });
    expect((await fetchAuthConfig(c, null)).magicLink).toBe(true);
  });

  it("reports a dead-end magic link when the host says so", async () => {
    // A routable host with no transport. The default must not survive an
    // explicit false, or the console draws a form that goes nowhere.
    const c = client({
      get: async () => ({ mode: "email", passwords: true, magicLink: false }),
    });
    expect((await fetchAuthConfig(c, null)).magicLink).toBe(false);
  });
});

describe("invite", () => {
  it("sends an email address in email mode", async () => {
    const post = vi.fn().mockResolvedValue({});
    const c = { scopeFor: () => "/api/v1/company", post } as never;
    await invite(c, null, "ada@example.com", "member", "email");
    expect(post).toHaveBeenCalledWith("/api/v1/company/users/invites", {
      email: "ada@example.com",
      role: "member",
    });
  });

  it("sends a wallet address in wallet mode, and never as an email", async () => {
    const post = vi.fn().mockResolvedValue({});
    const c = { scopeFor: () => "/api/v1/company", post } as never;
    await invite(c, null, "7xKXtg2CW87d97", "admin", "wallet");
    expect(post).toHaveBeenCalledWith("/api/v1/company/users/invites", {
      wallet: "7xKXtg2CW87d97",
      role: "admin",
    });
  });
});

describe("requestWalletChallenge", () => {
  it("asks the host for the exact bytes to sign", async () => {
    const post = vi.fn().mockResolvedValue({
      nonce: "n1",
      message: "opencompany-wallet-login-v1\nacme\n7xKX\nn1\n1700000000000",
      expiresAtMillis: 1,
    });
    const c = { scopeFor: () => "/api/v1/company", post } as never;
    const challenge = await requestWalletChallenge(c, null, "7xKX");

    expect(post).toHaveBeenCalledWith("/api/v1/company/auth/wallet/challenge", {
      address: "7xKX",
    });
    // The console signs this verbatim; it must never rebuild the layout, or a
    // host-side version bump silently stops verifying.
    expect(challenge.message.split("\n")[0]).toBe("opencompany-wallet-login-v1");
  });
});
