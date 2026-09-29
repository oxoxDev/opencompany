// The company's own TinyHumans credential (issue #586): the one key its admin
// sets on this tenant, and the identity every surface the platform brokers
// presents on the company's behalf.
//
// Set it once and Composio rides it — no separate Composio token, no per-tenant
// provider app to register. Rotate it and every brokered surface moves together,
// because they all resolve through one seam on the host rather than each keeping
// its own copy.
//
// WRITE-ONLY, like every other credential the console handles: the key goes out
// on `PUT .../credential`, lands in the host's secret store, and is never
// returned. The read shape carries only `configured` plus the non-secret tier
// name. Standalone functions over the shared client (mirrors `api/composio.ts`).
//
// NOT the same thing as the inference key in `api/inference.ts`. That one holds
// whatever the company's *declared provider* wants — an OpenRouter key, a raw
// BYOK token — so it is provider-scoped, not an identity, and handing it to the
// TinyHumans backend would present one vendor's credential to another.

import type { OpenCompanyClient } from "./client";
import type { UsedBy } from "./types";

/**
 * Which identity this company's brokered calls present right now.
 *
 * - `company` — this company's own key. What setting one buys you.
 * - `attested` / `static` — no company key set, so calls fall back to the
 *   instance's platform identity.
 * - `none` — neither, so nothing the platform brokers for this company works:
 *   no provider can be connected, and there is no TinyHumans account to bill
 *   thinking to. The honest degraded state, and the one the picker must not
 *   paper over — but it is **not** "nothing works". A company whose LLM page
 *   holds a key of its own thinks perfectly well with this unset: that key
 *   outranks this one in the managed chain, and a provider of its own does not
 *   consult this one at all.
 */
export type CompanyCredentialSource = "company" | "attested" | "static" | "none";

/** The company's credential status. Never carries the key. */
export interface CompanyCredentialStatus {
  /**
   * Whether this company has its **own** key stored. `false` does not mean no
   * credential — read {@link source} for what calls actually present.
   */
  configured: boolean;
  /** Which identity a brokered call presents right now. */
  source: CompanyCredentialSource;
  /**
   * The consequence of setting this key, or the degraded state when nothing can
   * be presented. Rendered verbatim: the host words it, so the console cannot
   * drift from what the host actually does.
   */
  notice: string;
  /**
   * Whether this host can complete a one-click key grant against the hub.
   *
   * `false` on every host with no hub wired — self-hosted, or a build with no
   * exchange — and the console then renders exactly what it rendered before this
   * flow existed: the paste field, alone. Carried on the status rather than
   * asked for separately so the decision costs no extra request, and absent on a
   * host predating the field, which `?? false` reads as "no button", the safe
   * direction.
   */
  hubLink?: boolean;
  /**
   * Where this person looks after the account behind the key — the hub
   * dashboard's key list, and its top-up page.
   *
   * Resolved by the **host**, because only the host knows which hub it was
   * pointed at: a console talking to staging must link to the staging
   * dashboard, and a link assembled in the browser would send an operator to
   * production's billing page. Absent on a host whose backend the naming
   * convention does not describe (self-hosted, loopback), where there is no
   * dashboard to link to — the console then renders no link rather than a
   * guess.
   */
  account?: HubAccountLinks;
  /**
   * The LLM TinyHumans key slot holds a key that is not the account key.
   * Saving leaves it alone (Q7). Never the key itself — only whether the two
   * stored values are equal. Optional: an older host omits it, which the
   * account-key dialog reads as "the host did not say" rather than `false`
   * (keys rework, issue #2306, slice 4b).
   */
  inferenceHasOwnKey?: boolean;
  /** The same for `composio/tinyhumans/key` (with 1a's legacy read). */
  composioHasOwnKey?: boolean;
  /** The same for the company-owned managed Search credential. */
  searchHasOwnKey?: boolean;
  /**
   * Whether the `tinyhumans` row saving would fill already carries a model —
   * so saving here would not leave anything for step two to ask, and the
   * dialog's fill line has nothing to promise about "finishing" LLM
   * (round-3b review, P3-4). Absent on a host that has not landed this fact
   * on `SlotFacts` yet, which `accountFills` reads the same way it reads a
   * missing `inferenceHasOwnKey` — "did not say", so the line keeps today's
   * wording rather than guessing a row has no model when it might.
   */
  inferenceHasModel?: boolean;
  /** `inference/default` is set (`ProviderOnly` or `Full`) — never overwritten by a save. */
  defaultSet?: boolean;
  /**
   * What a **clear** of this key would strand right now — the same shape a
   * refused clear's `409 in_use` echoes (`docs/key-reworks/in-use-guards.md`
   * §1-2). Carried on the status the page already reads so the Remove-key
   * dialog can name dependents the moment it opens, without waiting for a
   * stale, uninformed attempt to be refused first (keys rework #2306,
   * KR-L3-01). Absent when the key is unset or nothing would be stranded —
   * `"usedBy" in status` is itself the in-use check, same as every other DTO
   * this contract covers.
   */
  usedBy?: UsedBy;
}

/** The two hub pages the console links out to. */
export interface HubAccountLinks {
  /** The dashboard's API-key list — where a minted key is seen and revoked. */
  manageKeysUrl: string;
  /** The dashboard's balance and top-up page. */
  topUpUrl: string;
}

/** One of the five things a single `PUT …/credential` can touch. */
export type FanOutSlot = "composio" | "inference" | "provider" | "default" | "health";

/** What happened to one {@link FanOutSlot} (the wire spelling of `SlotOutcome`). */
export type FanOutOutcome =
  | "filled"
  | "rotated"
  | "cleared"
  | "rolledBack"
  | "kept"
  | "skipped"
  | "failed"
  | "ok";

/** One slot's report line, from `company_key::fan_out` (keys rework, issue #2306, slice 4a). */
export interface FanOutSlotReport {
  slot: FanOutSlot;
  outcome: FanOutOutcome;
  /** The camelCase skip reason, `"store"` for a plain failure, or a probe class for the health slot. */
  detail?: string;
}

/**
 * A mutating response: the resulting status plus a plain-language note.
 *
 * The fan-out fields (`slots` onward) are present on every host running the
 * keys rework's slice 4a or later; an older host omits all of them, which
 * degrades to today's single-step dialog (see `account-fill.ts`).
 */
export interface CompanyCredentialMutation {
  status: CompanyCredentialStatus;
  note: string;
  /** What the fan-out did to each of the five slots it touches, in order composio, inference, provider, default, health. */
  slots?: FanOutSlotReport[];
  /** Whether a `tinyhumans` row could not be created or defaulted for want of a model — the dialog's cue to ask for one. */
  needsModel?: boolean;
  /** Whether a model sent on a follow-up request would also become the company default. */
  setsDefault?: boolean;
  /** Catalog ids to offer, only ever alongside `needsModel`. */
  models?: string[];
  /**
   * Echoes what a **confirmed** clear would have refused with, computed
   * before the mutation applied. `undefined` on every mutation that is not a
   * guarded clear, and on a guarded one that had nothing to warn about.
   */
  usedBy?: UsedBy;
  /**
   * Whether the config this write just landed needs a restart before agents
   * actually run on it — same wire name and meaning as
   * `InferenceStatusDto.restartRequired` (`@/api/inference`), computed by the
   * same host-side function (KR-ACCT-01, 2026-09-15). The Account dialog has
   * no `cognition`/running-brain state of its own to compare against the way
   * the LLM page does, so this is carried directly on the mutation rather
   * than inferred: a save can create or complete a `tinyhumans` row that a
   * company already booted past, and only a restart puts it to work. Absent
   * (never `false`) on a host that predates this field, which the dialog
   * reads as "did not say" and — the safe direction here, since the fallback
   * is silence rather than a wrong guess — simply offers no restart action,
   * the same as it always has.
   */
  restartRequired?: boolean;
}

/** Whether this company has its own credential, and which identity it presents. */
export function getCompanyCredential(
  client: OpenCompanyClient,
  company: string | null,
): Promise<CompanyCredentialStatus> {
  return client.get<CompanyCredentialStatus>(`${client.scopeFor(company)}/credential`);
}

/**
 * Set / rotate / clear the company's TinyHumans credential. A non-empty value
 * sets or rotates it; an empty string clears it, falling back to the instance's
 * platform identity where there is one. Admin-only — a member gets a 403.
 *
 * `model` names the model a `tinyhumans` row should carry if the fan-out
 * creates one (keys rework, issue #2306, slice 4a) — the account-key dialog's
 * second step sends this on the follow-up save once the host has answered
 * `needsModel`. Omitted (never sent as `""`) while clearing or on the first
 * save, so an older host sees exactly the body it always has.
 *
 * A **clear** that would strand a dependent is refused with a `409 in_use`
 * `ApiError` carrying `usedBy` (in-use-guards.md §2) unless `confirmInUse` is
 * `true`. Setting or rotating a non-empty key is never guarded, so
 * `confirmInUse` matters only on an empty `key`. Omitted from the body
 * (rather than always sent as `false`, unlike Composio's equivalent calls)
 * when `false`, so every existing save/rotate body is unchanged — the
 * Remove-key dialog is the only caller that ever passes `true`, and only once
 * it has actually shown the operator a reason (`@/views/connections/
 * account-in-use`'s `confirmInUseFor`).
 */
export function setCompanyCredential(
  client: OpenCompanyClient,
  company: string | null,
  key: string,
  model?: string,
  confirmInUse = false,
): Promise<CompanyCredentialMutation> {
  return client.put<CompanyCredentialMutation>(`${client.scopeFor(company)}/credential`, {
    key,
    ...(model ? { model } : {}),
    ...(confirmInUse ? { confirmInUse: true } : {}),
  });
}

/**
 * Finish setting up TinyHumans for LLM with the account key the host already
 * holds — `PUT …/credential/model`. For a key the console cannot resend: a
 * key-grant (`finishCredentialLink`) stores one the page never saw and can
 * answer `needsModel`, and step two must then complete the row off the stored
 * key rather than off a `pendingKey` this page never had.
 */
export function setCompanyCredentialModel(
  client: OpenCompanyClient,
  company: string | null,
  model: string,
): Promise<CompanyCredentialMutation> {
  return client.put<CompanyCredentialMutation>(`${client.scopeFor(company)}/credential/model`, {
    model,
  });
}

/** The host's answer to `POST …/credential/link/start`. */
export interface CredentialLinkStart {
  authorizeUrl: string;
}

/**
 * Begin a one-click TinyHumans connection. Admin-only; 404 on a host with no hub.
 *
 * The returned URL is for the person's **browser**: the hub signs them in
 * through their provider and shows a consent screen, both on the hub's own
 * origin with its own address bar visible. In a browser tab that is a
 * top-level navigation; in the desktop it is handed to the system browser and
 * the host finishes the grant itself on its own return route.
 */
export function startCredentialLink(
  client: OpenCompanyClient,
  company: string | null,
): Promise<CredentialLinkStart> {
  return client.post<CredentialLinkStart>(`${client.scopeFor(company)}/credential/link/start`, {});
}

/**
 * Finish a connection: hand the host the code the hub returned, and the `state`
 * it started with.
 *
 * The host redeems these for a key and stores it as both the company credential
 * and the inference key. The response is the same shape a paste would have
 * produced, so the page that redeemed it can repaint from it either way.
 */
export function finishCredentialLink(
  client: OpenCompanyClient,
  company: string | null,
  state: string,
  code: string,
): Promise<CompanyCredentialMutation> {
  return client.post<CompanyCredentialMutation>(
    `${client.scopeFor(company)}/credential/link/finish`,
    { state, code },
  );
}

/** The account's money, as the API Key page draws it. */
export interface BillingSummary {
  /** Everything spendable — promotional credit and top-up together, in USD. */
  balanceUsd: number;
  /** The plan slug (`free`, `pro`, …). */
  plan: string;
  /** Whether a paid subscription is live right now. */
  activeSubscription: boolean;
  /** When the plan lapses, if it does. */
  planExpiry?: string;
  /** Where a person tops up, on the hub that issued the key. */
  topUpUrl?: string;
  /** Where a person changes the plan. */
  manageUrl?: string;
}

/**
 * Why the hub gave no figures, as the host classified it.
 *
 * - `rejected` — the hub refused the key (401). The one value that says the
 *   credential itself is dead.
 * - `unreachable` — the hub could not answer, or answered in a way this host
 *   could not use. Includes a 403: a key the hub recognises and will not let
 *   through is not a key to replace.
 * - `noHub` — this build is not wired to a hub at all, so there is nobody to
 *   ask. Says nothing about the key.
 * - `unknown` — the host could not tell which of the above it was.
 */
export type BillingUnavailableReason = "rejected" | "unreachable" | "noHub" | "unknown";

/**
 * The billing panel's whole state, including its two empty cases.
 *
 * `configured: false` is "no key, so nothing to ask about" — the page shows the
 * pitch. `unavailable` is "there is a key but the hub would not answer", which
 * is deliberately not the same as a zero balance: they look identical on a card
 * and mean opposite things, one "top up" and one "try again".
 */
export interface CompanyBilling {
  configured: boolean;
  summary?: BillingSummary;
  /**
   * The host's own sentence for why there are no figures. Never the hub's
   * response body, which is JSON written for a log.
   */
  unavailable?: string;
  /**
   * Which kind of failure it was — the field to switch on, so what the console
   * draws never depends on parsing prose.
   *
   * Absent on a host predating the field, which reads as "did not say" rather
   * than any of the four (the idiom `inferenceHasOwnKey` and `restartRequired`
   * already use). A missing reason must never be read as a working key: a
   * verdict is earned by evidence, and absence is not evidence.
   */
  unavailableReason?: BillingUnavailableReason;
  /** The hub's stable failure token (`http_401`, `unreachable`, …). */
  unavailableCode?: string;
}

/**
 * What the account behind this company's key has left to spend.
 *
 * Read through the **host**, which presents the key it holds — the console
 * never sees the credential, so it could not ask the hub itself. A read and
 * only a read: topping up and changing plans happen signed in on the hub.
 */
export function getCompanyBilling(
  client: OpenCompanyClient,
  company: string | null,
): Promise<CompanyBilling> {
  return client.get<CompanyBilling>(`${client.scopeFor(company)}/credential/billing`);
}
