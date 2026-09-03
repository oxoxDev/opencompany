import { describe, expect, it } from "vitest";

import { OpenCompanyClient } from "@/api/client";
import { lifecycleAffordances } from "@/lib/lifecycle-controls";

/**
 * Which lifecycle buttons the console is allowed to offer (issue #1401, plus
 * the admin-authority gate added alongside the server-side fix that made
 * `pause`/`resume` `AdminScopedCompany` routes).
 *
 * The original bug this pinned was not a wrong result, it was a *reachable*
 * control: the console rendered `Archive` — destructive, behind a dialog
 * calling itself permanent — to an operator signed in with a magic link, took
 * the confirmation, and then answered `401 unauthorized`, because `archive`
 * is a `PlatformScope` route a session cookie can never reach. The
 * admin-authority gate is the same failure mode one layer down: an ordinary
 * member reaches `pause`/`resume`, which now answer `403` unless the caller
 * is an admin. So most assertions here are about a button *not* existing,
 * which is the only form the fix can take.
 */

/** A client as the console builds one for a person: cookie session, no bearer. */
function humanClient(): OpenCompanyClient {
  return new OpenCompanyClient({
    baseUrl: "",
    company: "acme",
    operatorToken: null,
    sessionHeader: null,
  });
}

/** A client as a hosting console builds one: `?token=` / `VITE_OC_TOKEN`. */
function platformClient(): OpenCompanyClient {
  return new OpenCompanyClient({
    baseUrl: "",
    company: "acme",
    operatorToken: "platform-jwt",
    sessionHeader: null,
  });
}

describe("what the console knows about its own credential", () => {
  it("reports no platform bearer for the ordinary signed-in human", () => {
    // The normal deployment. The session rides in an HttpOnly cookie that
    // nothing in the bundle can read, and `resolve_claims` cannot turn it into
    // platform claims whatever it contains.
    expect(humanClient().carriesPlatformBearer).toBe(false);
  });

  it("reports a platform bearer when one was configured", () => {
    expect(platformClient().carriesPlatformBearer).toBe(true);
  });

  it("treats an empty token as no token", () => {
    // `?token=` with nothing after it resolves to `""`, which would be sent as
    // `Bearer ` and refused. A truthiness check keeps the button decision and
    // the header decision reading the same value the same way.
    const blank = new OpenCompanyClient({
      baseUrl: "",
      company: "acme",
      operatorToken: "",
      sessionHeader: null,
    });
    expect(blank.carriesPlatformBearer).toBe(false);
  });
});

describe("a console without a platform bearer, held by a member", () => {
  it("never offers suspend or archive, in any lifecycle", () => {
    for (const state of ["running", "paused", "suspended"]) {
      const { actions } = lifecycleAffordances(state, false, false);
      expect(actions).not.toContain("suspend");
      expect(actions).not.toContain("archive");
    }
  });

  it("withholds pause on a running company — pause is admin-scoped", () => {
    // The server-side fix: `pause` is `AdminScopedCompany`, so a member's
    // session reaches the route and is refused with `403`. A member must
    // never see a button whose only outcome is that toast.
    expect(lifecycleAffordances("running", false, false).actions).toEqual([]);
  });

  it("withholds resume on a paused company for the same reason", () => {
    expect(lifecycleAffordances("paused", false, false).actions).toEqual([]);
  });

  it("explains that pause/resume need admin authority, not the platform-only reason", () => {
    const shown = lifecycleAffordances("running", false, false);
    expect(shown.explainAdminOnly).toBe(true);
    expect(shown.explainPlatformOnly).toBe(false);
  });
});

describe("a console without a platform bearer, held by an admin", () => {
  it("still offers pause on a running company", () => {
    // The point of the fix is not to empty the card for everyone. `pause` is
    // reachable by an admin's session and is the operator's real, reversible
    // stop.
    expect(lifecycleAffordances("running", true, false).actions).toEqual(["pause"]);
  });

  it("still offers resume on a paused company", () => {
    expect(lifecycleAffordances("paused", true, false).actions).toEqual(["resume"]);
  });

  it("withholds resume on a platform-suspended company, and says why", () => {
    // `resume` is reachable by an admin session, but the handler refuses a
    // non-platform caller specifically when the lifecycle is `suspended`,
    // because that state is a platform-forced pause.
    const { actions, explainPlatformSuspended } = lifecycleAffordances("suspended", true, false);
    expect(actions).toEqual([]);
    expect(explainPlatformSuspended).toBe(true);
  });

  it("explains suspend/archive are withheld rather than dropping them silently", () => {
    const shown = lifecycleAffordances("running", true, false);
    expect(shown.explainPlatformOnly).toBe(true);
    expect(shown.explainAdminOnly).toBe(false);
  });
});

describe("a console holding a platform bearer", () => {
  it("offers suspend and archive regardless of the member/admin role", () => {
    for (const admin of [false, true]) {
      const { actions, explainPlatformOnly, explainAdminOnly } = lifecycleAffordances(
        "running",
        admin,
        true,
      );
      expect(actions).toEqual(["pause", "suspend", "archive"]);
      expect(explainPlatformOnly).toBe(false);
      expect(explainAdminOnly).toBe(false);
    }
  });

  it("offers resume on a suspended company, because it can lift one", () => {
    expect(lifecycleAffordances("suspended", false, true).actions).toContain("resume");
    expect(lifecycleAffordances("suspended", false, true).explainPlatformSuspended).toBe(false);
  });
});

describe("an archived company", () => {
  it("offers nothing to anyone, and explains nothing away", () => {
    // Terminal: the host removes it from the registry. Even a platform bearer
    // has no transition left, and the banners would be noise next to the
    // "This company is archived." line the card already shows.
    for (const admin of [false, true]) {
      for (const platform of [false, true]) {
        const shown = lifecycleAffordances("archived", admin, platform);
        expect(shown.actions).toEqual([]);
        expect(shown.archived).toBe(true);
        expect(shown.explainPlatformOnly).toBe(false);
        expect(shown.explainPlatformSuspended).toBe(false);
        expect(shown.explainAdminOnly).toBe(false);
      }
    }
  });
});

describe("a lifecycle string the console does not know", () => {
  it("offers no transition rather than guessing one", () => {
    // A host newer than this bundle can report a state that is not in the set.
    // Showing nothing is recoverable; showing Archive is not.
    expect(lifecycleAffordances("provisioning", true, true).actions).toEqual([
      "suspend",
      "archive",
    ]);
    expect(lifecycleAffordances("provisioning", true, false).actions).toEqual([]);
    expect(lifecycleAffordances("provisioning", false, false).actions).toEqual([]);
  });
});
