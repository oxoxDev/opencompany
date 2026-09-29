import { useEffect, useState } from "react";
import { ExternalLink, Loader2, Sparkles } from "lucide-react";

import type { OpenCompanyClient } from "@/api/client";
import { Button, buttonVariants } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import { ModelField } from "@/inference/ModelField";
import {
  COMPOSIO_PAGE_HREF,
  LLM_PAGE_HREF,
  SEARCH_PAGE_HREF,
  accountFillLine,
  modelStepTitle,
  type AccountFills,
} from "@/views/connections/account-fill";

/** The second step's shape, once the host has answered `needsModel` (keys rework #2306, 4a/4b). */
export interface AccountKeyModelStep {
  /** Catalog ids to offer — may be empty, in which case {@link ModelField} falls back to free text. */
  models: string[];
  /** Whether the model chosen here would also become the company default. */
  setsDefault: boolean;
  /** The host's own note from the first save, shown verbatim above the field. */
  note: string;
}

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Whether a key of this company's own is already stored — changes the verb. */
  replacing: boolean;
  busy: boolean;
  /**
   * Why the last save failed, in the host's words where it sent some. Shown
   * inside the dialog, beside the field it is about, rather than as a toast
   * that disappears while the operator is still looking at the key they typed.
   */
  error: string | null;
  /** Saves the pasted value. The page closes the dialog once the write lands. */
  onSubmit: (key: string) => void;
  /** Which of the LLM/Composio/Search slots this save would fill — `null` renders no line. */
  fills: AccountFills | null;
  /** Set once the host answers `needsModel` for the key just saved — the dialog then shows step two. */
  modelStep: AccountKeyModelStep | null;
  /** Saves the model chosen in step two, against the same key already saved in step one. */
  onSubmitModel: (model: string) => void;
  /** Needed only by step two's {@link ModelField}, which takes list mode and fetches nothing. */
  client: OpenCompanyClient;
  company: string | null;
  /**
   * Starts the one-click grant (`hubLink` on the status): the person signs in
   * on the hub and the key arrives without being copied. `null` on a host with
   * no hub, or for someone who may not change the credential — the dialog then
   * offers the paste field alone, exactly as before the button existed.
   */
  onConnect: (() => void) | null;
  /** Whether a grant is in flight — the button waits, the field stays usable. */
  connecting: boolean;
  /**
   * The API-keys page of the hub this host is on, from the status
   * (`account.manageKeysUrl`). Where a key is minted by hand; never a
   * production constant, because a key minted there would be refused by a
   * staging host. `null` when the host derives no site, and then the paste
   * field carries no link.
   */
  keysUrl: string | null;
}

/**
 * "Connect to TinyHumans" — the Account page's API-key option.
 *
 * The same ask the setup wizard makes for Managed — a key field and, for the
 * operator who has none, a link to where one is created — plus what the
 * wizard cannot offer: the one-click grant, which needs a company to scope
 * the key to and this page has one. The link is the hub **this host is on**
 * (`keysUrl`, from the status), not a constant: the console used to send a
 * staging host's operator to mint a key on production
 * (`TINYHUMANS_API_KEYS_URL` in `@/lib/links` survives only as the wizard's fallback for
 * a host too old to report its own).
 *
 * Deliberately minimal (operator request, 2026-09-14): a heading, the field,
 * the "Get an API key" link, Save and Cancel, and an error only when a save
 * fails — plus, since the keys rework (issue #2306), one conditional line
 * naming the LLM/Composio/Search slots this save would fill (Q9) and, when the host
 * answers `needsModel`, a second step asking for the model to finish setting
 * up TinyHumans for LLM. Still no other explanatory paragraph.
 *
 * ## What it writes
 *
 * `PUT …/credential`, under a per-company lock
 * (`company_key::fan_out`, slice 4a): the account key itself, and — never
 * overwriting a key set on that page's own (Q7) — its copies at
 * `composio/tinyhumans/key`, `provider/tinyhumans/key`, and
 * `search/managed/key`. A `tinyhumans` row
 * is only ever created with a model (a key with no row is not "set" — see
 * `account-fill.ts`), which is what step two is for.
 *
 * Write-only, like every credential the console handles: the value is never
 * returned, so the field opens empty every time and "set" is reported by a
 * flag rather than by a masked value we would have had to receive.
 */
export function AccountKeyDialog({
  open,
  onOpenChange,
  replacing,
  busy,
  error,
  onSubmit,
  fills,
  modelStep,
  onSubmitModel,
  client,
  company,
  onConnect,
  connecting,
  keysUrl,
}: Props) {
  const [key, setKey] = useState("");
  const [model, setModel] = useState("");

  // Cleared whenever the dialog opens or closes. A credential left in component
  // state after a save is a credential sitting in a heap snapshot for no
  // reason, and reopening on the previous paste would let a second Save write a
  // value the operator thinks they have already used. `model` gets the same
  // treatment — it is never sensitive, but a stale value from a previous save
  // reopening on step two would be its own kind of surprise.
  useEffect(() => {
    setKey("");
    setModel("");
  }, [open]);

  const fillLine = accountFillLine(fills);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        {modelStep ? (
          <>
            <DialogHeader>
              <DialogTitle>{modelStepTitle(modelStep.setsDefault)}</DialogTitle>
            </DialogHeader>

            <form
              className="space-y-4"
              onSubmit={(event) => {
                event.preventDefault();
                onSubmitModel(model.trim());
              }}
            >
              <p className="text-xs text-muted-foreground" data-testid="account-key-note">
                {modelStep.note}
              </p>

              <div data-testid="account-key-model-step">
                <ModelField
                  client={client}
                  company={company}
                  slug={null}
                  id="account-key-model"
                  label="Model"
                  value={model}
                  onChange={setModel}
                  models={modelStep.models}
                  catalogError={
                    modelStep.models.length === 0
                      ? "TinyHumans did not list its models, so type an id."
                      : undefined
                  }
                />
              </div>

              <p
                aria-live="polite"
                id="account-key-error"
                className="text-sm text-status-blocked-text empty:hidden"
                data-testid="account-key-error"
              >
                {error ?? ""}
              </p>

              <DialogFooter>
                {/* The key and its Composio copy are already saved (step one
                    landed before step two ever opens), so Cancel here closes
                    the dialog rather than rolling anything back. Disabled
                    while busy (round-3b review, P2-4) — the parent's own
                    `onOpenChange` guard already refuses the close, but a
                    button that visibly does nothing on click is its own kind
                    of confusing. */}
                <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>
                  Cancel
                </Button>
                <Button
                  type="submit"
                  disabled={busy || !model.trim()}
                  data-testid="account-key-model-save"
                >
                  {busy ? <Loader2 className="size-4 animate-spin" /> : null}
                  Save model
                </Button>
              </DialogFooter>
            </form>
          </>
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>{replacing ? "Replace your API key" : "Connect to TinyHumans"}</DialogTitle>
            </DialogHeader>

            <form
              className="space-y-4"
              onSubmit={(event) => {
                event.preventDefault();
                onSubmit(key.trim());
              }}
            >
              {/* The one-click way first, where the host can offer it: the
                  same PKCE grant the wizard cannot run (no company yet), which
                  this page has. The key is minted for this company and stored
                  by the host — nothing to copy, nothing shown. */}
              {onConnect && (
                <div className="space-y-2" data-testid="account-key-connect">
                  <Button
                    type="button"
                    className="w-full"
                    disabled={busy || connecting}
                    onClick={onConnect}
                    data-testid="connect-tinyhumans"
                  >
                    {connecting ? (
                      <Loader2 className="size-4 animate-spin" />
                    ) : (
                      <Sparkles className="size-4" />
                    )}
                    {replacing ? "Reconnect with TinyHumans" : "Connect with TinyHumans"}
                  </Button>
                  <p className="text-xs text-muted-foreground">
                    Sign in to TinyHumans and this company gets its key automatically — nothing
                    to copy.
                  </p>
                  <p className="text-2xs font-medium uppercase tracking-wide text-muted-foreground">
                    or
                  </p>
                </div>
              )}

              <div className="grid gap-1.5">
                <Label htmlFor="company-credential">
                  {onConnect ? "Paste an API key" : "Add your API key"}
                </Label>
                <Input
                  id="company-credential"
                  type="password"
                  autoComplete="off"
                  spellCheck={false}
                  aria-describedby={error ? "account-key-error" : undefined}
                  value={key}
                  onChange={(event) => setKey(event.target.value)}
                  data-testid="account-key-input"
                />
                {keysUrl && (
                  <p className="flex items-center gap-2 text-xs text-muted-foreground">
                    Don&apos;t have an API key?
                    <a
                      href={keysUrl}
                      target="_blank"
                      rel="noreferrer"
                      data-testid="account-key-get-link"
                      className={cn(buttonVariants({ variant: "outline", size: "sm" }))}
                    >
                      Get an API key
                      <ExternalLink className="size-3.5" />
                    </a>
                  </p>
                )}
                {fillLine && (
                  <p className="text-xs text-muted-foreground" data-testid="account-key-fill-line">
                    {/* `llmShown` matches `accountFillLine`'s own `llm` gate
                        exactly (round-3b review, P3-4) — a row that already
                        has a model gets no LLM clause in the sentence, so it
                        must get no dangling "LLM page" link either. */}
                    {fillLine}{" "}
                    {fills?.llm && !fills?.llmHasModel && (
                      <a
                        href={LLM_PAGE_HREF}
                        data-testid="account-key-llm-link"
                        className="font-medium text-foreground underline underline-offset-4"
                      >
                        LLM page
                      </a>
                    )}
                    {fills?.llm && !fills?.llmHasModel && fills?.composio && " · "}
                    {fills?.composio && (
                      <a
                        href={COMPOSIO_PAGE_HREF}
                        data-testid="account-key-composio-link"
                        className="font-medium text-foreground underline underline-offset-4"
                      >
                        Composio page
                      </a>
                    )}
                    {(fills?.llm && !fills?.llmHasModel && fills?.search) ||
                    (fills?.composio && fills?.search)
                      ? " · "
                      : null}
                    {fills?.search && (
                      <a
                        href={SEARCH_PAGE_HREF}
                        data-testid="account-key-search-link"
                        className="font-medium text-foreground underline underline-offset-4"
                      >
                        Search page
                      </a>
                    )}
                  </p>
                )}
              </div>

              {/* Always present, filled only on failure: a live region mounted at the
                  same moment as its text is frequently not announced. */}
              <p
                aria-live="polite"
                id="account-key-error"
                className="text-sm text-status-blocked-text empty:hidden"
                data-testid="account-key-error"
              >
                {error ?? ""}
              </p>

              <DialogFooter>
                {/* Disabled while busy (round-3b review, P2-4) — same reason
                    as step two's Cancel button above. */}
                <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>
                  Cancel
                </Button>
                <Button type="submit" disabled={busy || !key.trim()} data-testid="account-key-save">
                  {busy ? <Loader2 className="size-4 animate-spin" /> : null}
                  {replacing ? "Replace key" : "Save key"}
                </Button>
              </DialogFooter>
            </form>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
