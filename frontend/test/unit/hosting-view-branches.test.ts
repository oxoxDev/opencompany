// @vitest-environment jsdom

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ApiError } from "@/api/types";
import type { OpenCompanyClient } from "@/api/client";
import { HostingView } from "@/views/HostingView";

/**
 * The conditional surfaces of `HostingView`, which are where its whole job is.
 *
 * Three separate things can each stop a teammate deploying, and two of them are
 * invisible from this form's own fields: the company not granting `hosting`, and
 * the host being built without the tools at all. A single "Connected" badge
 * would be green for both and send an operator hunting through a form that is
 * already correct — so each is reported on its own terms, and the ordering
 * between them matters: granting `hosting` fixes nothing on a host compiled
 * without the harness.
 */

const HOSTING_OK = {
  apiKeyConfigured: true,
  provider: "vercel",
  team: "team_abc",
  granted: true,
  inBuild: true,
  supportedProviders: ["vercel"],
};

/**
 * A client answering the hosting read with a value or a rejection.
 *
 * `/auth/me` is answered separately, and as an admin by default: since #1796
 * this page resolves the viewer's role to decide whether to offer the grant
 * control, and a client that rejected every `GET` alike would leave every test
 * here asserting a non-admin's view by accident. `session: "none"` rejects the
 * way the host's own `no_session()` does — a real `ApiError` from its
 * `{error, code}` envelope — for an unauthenticated or bearer-only caller.
 * `session: "error"` rejects with a plain network-style error: not a
 * confirmed absence of a session, so it must not be read as one.
 * `carriesPlatformBearer` is independent of it — a hub console can carry a
 * bearer alongside a real session (`authHeaders`'s own doc comment).
 */
function clientWith(
  answer: unknown,
  session: "admin" | "member" | "none" | "error" = "admin",
  carriesPlatformBearer = false,
): OpenCompanyClient {
  return {
    scopeFor: () => "/api/v1/companies/acme",
    carriesPlatformBearer,
    get: (path: string) => {
      if (path.endsWith("/auth/me")) {
        if (session === "none") return Promise.reject(new ApiError(401, "unauthorized", "not signed in", true));
        if (session === "error") return Promise.reject(new Error("network down"));
        return Promise.resolve({ id: "u1", email: "a@b.c", role: session, company: "acme", hasPassword: true });
      }
      return answer instanceof Error ? Promise.reject(answer) : Promise.resolve(answer ?? HOSTING_OK);
    },
  } as unknown as OpenCompanyClient;
}

let container: HTMLDivElement;
let root: Root;

async function show(client: OpenCompanyClient) {
  await act(async () => {
    root.render(createElement(HostingView, { client, company: "acme" }));
  });
}

function at(testid: string): HTMLElement | null {
  return container.querySelector<HTMLElement>(`[data-testid="${testid}"]`);
}

beforeEach(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
});

describe("HostingView status surfaces", () => {
  it("renders the connected badge and neither alert on the ordinary path", async () => {
    // The control: the assertions below are only worth having if the working
    // case does not produce either alert.
    await show(clientWith(HOSTING_OK));

    expect(at("hosting-view")).not.toBeNull();
    expect(at("hosting-connected")).not.toBeNull();
    expect(at("hosting-not-granted")).toBeNull();
    expect(at("hosting-not-in-build")).toBeNull();
  });

  it("names the grant, not the form, when the company does not grant hosting", async () => {
    // A token stored and still nothing reaches a teammate, so saying "not
    // connected" here would send the operator back through a form that is
    // already correct.
    await show(clientWith({ ...HOSTING_OK, granted: false }));

    expect(at("hosting-not-granted")?.textContent).toContain("hosting");
    expect(at("hosting-not-in-build")).toBeNull();
    // The token is fully configured (HOSTING_OK) and the badge must still not
    // agree with a page that just said this integration reaches nobody.
    expect(at("hosting-connected")).toBeNull();
  });

  it("offers to grant the namespace rather than dead-ending (issue #1796)", async () => {
    // This page used to end the sentence with "it cannot be fixed from this
    // page" — true when written, and the whole of the bug: on a hosted tenant
    // the manifest is a read-only boot snapshot, so the operator had nowhere
    // left to go and the integration read "Connected" forever.
    await show(clientWith({ ...HOSTING_OK, granted: false }));

    const action = at("hosting-not-granted-action");
    expect(action).not.toBeNull();
    expect(action?.textContent).toContain("Grant hosting");
    expect(at("hosting-not-granted")?.textContent).not.toContain(
      "cannot be fixed from this page",
    );
  });

  it("offers a non-admin the explanation and no control", async () => {
    // Every write behind the control is admin-only, so a member gets told what
    // is wrong and who can fix it. Offering the button anyway would trade the
    // old dead end for a button whose only possible outcome is a 403 toast.
    await show(clientWith({ ...HOSTING_OK, granted: false }, "member"));

    const warning = at("hosting-not-granted");
    expect(warning).not.toBeNull();
    expect(warning?.textContent).toContain("An admin has to grant it");
    expect(at("hosting-not-granted-action")).toBeNull();
  });

  it("says the host lacks the tools, and says only that", async () => {
    // Not-in-build outranks not-granted: granting `hosting` in the manifest
    // fixes nothing on a host compiled without the harness, and showing both
    // alerts gives two remedies for one problem.
    await show(clientWith({ ...HOSTING_OK, granted: false, inBuild: false }));

    expect(at("hosting-not-in-build")).not.toBeNull();
    expect(at("hosting-not-granted")).toBeNull();
    expect(at("hosting-connected")).toBeNull();
  });

  it("shows the page-level error when the status cannot be read", async () => {
    await show(clientWith(new Error("store unreachable")));

    expect(at("hosting-load-error")?.textContent).toContain("store unreachable");
    expect(at("hosting-api-key")).toBeNull();
  });

  it("never renders a stored token, and offers to replace rather than reveal it", async () => {
    // The token is write-only end to end: the host does not return it, so the
    // field stays empty with a placeholder. An input pre-filled with dots would
    // invite an operator to "correct" a value they cannot see.
    await show(clientWith(HOSTING_OK));

    const key = at("hosting-api-key") as HTMLInputElement | null;
    expect(key?.value).toBe("");
    expect(key?.getAttribute("placeholder")).toContain("Configured");
    expect(key?.getAttribute("type")).toBe("password");
  });

  it("seeds the team box with what is stored so a typo can be corrected", async () => {
    // The one non-secret field. Leaving it empty would make an operator retype
    // it to change anything else.
    await show(clientWith(HOSTING_OK));

    expect((at("hosting-team") as HTMLInputElement | null)?.value).toBe("team_abc");
  });

  it("offers no disconnect button before anything is stored", async () => {
    await show(clientWith({ ...HOSTING_OK, apiKeyConfigured: false, team: null }));

    expect(at("hosting-connected")).toBeNull();
    expect(at("hosting-clear")).toBeNull();
    expect(at("hosting-save")).not.toBeNull();
  });
});

describe("HostingView authority (issue #1785 copy-paste pair)", () => {
  it("offers a member the credential fields, and no way to submit them", async () => {
    // The provider picker's sibling defect: the API token field, the team
    // field and Save were rendered enabled for a member, and the host refuses
    // `PUT …/hosting` with a 403 whatever the console shows. Presence, not
    // enabledness — matching `connections-authority.spec.ts`.
    await show(clientWith(HOSTING_OK, "member"));

    expect(at("hosting-read-only")?.textContent).toContain("Only an admin");
    expect(at("hosting-api-key")).toBeNull();
    expect(at("hosting-team")).toBeNull();
    expect(at("hosting-save")).toBeNull();
    expect(at("hosting-clear")).toBeNull();

    // A member is not shown a blank page: the current connection state is
    // still on screen.
    expect(at("hosting-connected")).not.toBeNull();
    expect(at("hosting-view")?.textContent).toContain("vercel");
  });

  it("tells a member a token is stored even when connected collapses that with a build/grant gap", async () => {
    // `connected` folds apiKeyConfigured, granted and inBuild into one flag,
    // so a stored token with the manifest not yet granting `hosting` reads as
    // "Not connected yet" — indistinguishable, for a member reading only that
    // summary, from no token ever having been saved (codex review).
    await show(clientWith({ ...HOSTING_OK, granted: false }, "member"));

    expect(at("hosting-connected")).toBeNull();
    expect(at("hosting-credential-status")?.textContent).toContain("An API token is stored");
  });

  it("names the team scope in the member's credential-status line when one is set", async () => {
    await show(clientWith({ ...HOSTING_OK, granted: false, team: "team_abc" }, "member"));

    expect(at("hosting-credential-status")?.textContent).toContain("team_abc");
  });

  it("offers an admin every control, with no read-only notice", async () => {
    await show(clientWith(HOSTING_OK, "admin"));

    expect(at("hosting-read-only")).toBeNull();
    expect(at("hosting-api-key")).not.toBeNull();
    expect(at("hosting-team")).not.toBeNull();
    expect((at("hosting-save") as HTMLButtonElement | null)?.disabled).toBe(false);
  });

  it("offers a bearer-only caller every control once /auth/me finds no session", async () => {
    await show(clientWith(HOSTING_OK, "none", true));

    expect(at("hosting-read-only")).toBeNull();
    expect(at("hosting-api-key")).not.toBeNull();
  });

  it("stays read-only on an ambiguous /auth/me failure, even with a bearer present", async () => {
    // A network error, a timeout, or a 5xx is not a confirmed absence of a
    // session — a member's session could still be live and would still take
    // precedence on the host (coderabbit review).
    await show(clientWith(HOSTING_OK, "error", true));

    expect(at("hosting-read-only")?.textContent).toContain("Only an admin");
    expect(at("hosting-api-key")).toBeNull();
  });

  it("defers to a member session even when a platform bearer is also present", async () => {
    // A hub console can carry both credentials at once (`authHeaders`), and
    // resolve_principal tries the session first — so a bearer must never
    // paper over a member's own 403 (codex review).
    await show(clientWith(HOSTING_OK, "member", true));

    expect(at("hosting-read-only")?.textContent).toContain("Only an admin");
    expect(at("hosting-api-key")).toBeNull();
  });

  it("defers to an admin session when a platform bearer is also present", async () => {
    await show(clientWith(HOSTING_OK, "admin", true));

    expect(at("hosting-read-only")).toBeNull();
    expect(at("hosting-api-key")).not.toBeNull();
  });
});
