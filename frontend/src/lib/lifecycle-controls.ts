import type { LifecycleAction } from "@/api/client";

/**
 * Which lifecycle controls the console may honestly offer (issue #1401).
 *
 * The four buttons in `Settings → General → Lifecycle` do not share an
 * authorization story, and the console used to render them as if they did:
 *
 * - `pause` / `resume` are `CompanyAuth` routes gated by `AdminScopedCompany`:
 *   a person signed in with a magic link reaches them only when their role on
 *   this company is `admin` — an ordinary member is refused with `403`.
 * - `suspend` / `archive` are `PlatformScope` routes. That extractor resolves
 *   through `resolve_claims`, which cannot return a human, so a session cookie
 *   can never reach them *whatever it contains*. The console only ever holds
 *   platform scope when it was handed a platform bearer (`?token=` /
 *   `VITE_OC_TOKEN`), which is not how somebody signing in with a magic link
 *   arrives.
 *
 * So `Archive` — styled `destructive`, behind a dialog calling itself
 * permanent — took the confirmation and then answered `401 unauthorized`. That
 * is the one failure mode this console is otherwise careful about: Billing and
 * Hosting both say, in the page, when a control cannot work here. Lifecycle
 * instead invited an irreversible decision it could not carry out.
 *
 * `platform` here means *this client sends a platform bearer*, not *that bearer
 * carries the scope* — a tenant token without `platform` still gets a `403`.
 * That residue is deliberate: a wrong-scope token is a **configuration**
 * mistake an operator can fix, whereas a session cookie is refused **by
 * construction**, and only the second one is worth hiding a button over.
 */
export interface LifecycleAffordances {
  /** The actions whose buttons may be rendered, in display order. */
  actions: LifecycleAction[];
  /**
   * Whether to explain that suspend and archive were withheld.
   *
   * Withholding them silently would trade a dishonest button for a missing
   * one, and an operator who read the docs would go looking for the control
   * rather than learning it is not theirs.
   */
  explainPlatformOnly: boolean;
  /**
   * Whether to explain that this company's `suspended` state is not the
   * operator's to lift.
   *
   * `resume` is a `CompanyAuth` route, so the button is reachable — but the
   * handler refuses a non-platform caller specifically when the lifecycle is
   * `suspended`, because that state is a platform-forced pause. Rendering
   * `Resume` there is the same dishonesty as `Archive`, one layer deeper.
   */
  explainPlatformSuspended: boolean;
  /**
   * Whether to explain that pause and resume need admin authority here.
   *
   * A signed-in member reaches the same routes as an admin — `AdminScopedCompany`
   * refuses them with `403`, it does not hide the route — so the console must
   * withhold the button itself rather than let a click end in that toast.
   */
  explainAdminOnly: boolean;
  /** Whether the company is past the end of its lifecycle. */
  archived: boolean;
}

/**
 * @param lifecycle the host's `status.lifecycle` (or the optimistic pending one)
 * @param admin whether the signed-in caller holds the `admin` role on this company
 * @param platform whether this client carries a platform bearer
 */
export function lifecycleAffordances(
  lifecycle: string,
  admin: boolean,
  platform: boolean,
): LifecycleAffordances {
  const archived = lifecycle === "archived";
  const suspended = lifecycle === "suspended";
  if (archived) {
    return {
      actions: [],
      explainPlatformOnly: false,
      explainPlatformSuspended: false,
      explainAdminOnly: false,
      archived: true,
    };
  }

  const authorized = admin || platform;
  const actions: LifecycleAction[] = [];
  if (authorized) {
    if (lifecycle === "running") actions.push("pause");
    // A paused company is any admin's to restart; a suspended one is the platform's.
    if (lifecycle === "paused" || (suspended && platform)) actions.push("resume");
  }
  if (platform) actions.push("suspend", "archive");

  return {
    actions,
    explainPlatformOnly: authorized && !platform,
    explainPlatformSuspended: authorized && suspended && !platform,
    explainAdminOnly: !authorized,
    archived: false,
  };
}
