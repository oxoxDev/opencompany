// The review an operator sees before a newer library document replaces the one
// their agents are reading.
//
// Owned by `SkillsView` like the other three dialogs: the view holds the client,
// the registry it compares against, and the list the answer folds back into.
//
// What it deliberately does **not** show is the document itself. No route serves
// a skill's full text — `GET …/skills` and the registry listing are both metadata
// only — so a diff here would be invented. The dialog says so in a line rather
// than leaving the operator to assume the body is unchanged because nothing
// showed it changing.
//
// Keep writes nothing. There is no snooze flag on the host and none here, so the
// badge comes back on the next read. That is the honest behaviour for a library
// that has genuinely moved on, and it is why the button says Keep rather than
// Dismiss.

import { useState } from "react";
import { Loader2 } from "lucide-react";
import { toast } from "sonner";

import type { OpenCompanyClient } from "@/api/client";
import { updateSkill, type RegistrySkill, type Skill } from "@/api/skills";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { skillSourceLabel } from "@/lib/skills-list";

/** A revision as a label, falling back to a word rather than to a blank cell.
 *
 * An unversioned document is ordinary — `version` is optional frontmatter — and
 * an empty cell beside a filled one reads as data the console failed to load. */
function revision(value: string | null | undefined): string {
  const text = typeof value === "string" ? value.trim() : "";
  if (!text) return "unversioned";
  return text.toLowerCase().startsWith("v") ? text : `v${text}`;
}

export function UpdateSkillDialog({
  client,
  company,
  skill,
  registry,
  onOpenChange,
  onUpdated,
}: {
  client: OpenCompanyClient;
  company: string | null;
  /** The row under review, or `null` when the dialog is closed. */
  skill: Skill | null;
  /** The live registry the view already holds, so the comparison names what the
   * library says now rather than asking the host a second time. */
  registry: RegistrySkill[];
  onOpenChange: (open: boolean) => void;
  onUpdated: (saved: Skill) => void;
}) {
  const [busy, setBusy] = useState(false);
  const live = skill ? registry.find((entry) => entry.id === skill.id) : undefined;

  async function apply() {
    if (!skill) return;
    setBusy(true);
    try {
      onUpdated(await updateSkill(client, company, skill.id));
      onOpenChange(false);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "could not update the skill");
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={skill !== null} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg" data-testid="skill-update-dialog">
        <DialogHeader>
          <DialogTitle>Update {skill?.name ?? "this skill"}?</DialogTitle>
          <DialogDescription>
            The registry has a newer version of this skill. Updating replaces the copy your
            teammates read.
          </DialogDescription>
        </DialogHeader>

        <p className="text-sm" data-testid="skill-update-versions">
          {revision(skill?.updateAvailable?.from ?? skill?.version)} →{" "}
          <span className="font-medium">{revision(skill?.updateAvailable?.to)}</span>
        </p>

        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
          <dt className="text-muted-foreground">Installed</dt>
          <dd data-testid="skill-update-installed">
            {skill ? skillSourceLabel(skill) : ""}
            {skill?.description ? ` · ${skill.description}` : ""}
          </dd>
          <dt className="text-muted-foreground">In the registry</dt>
          <dd data-testid="skill-update-live">
            {live
              ? `${live.publisher} ${revision(live.version)}${
                  live.description ? ` · ${live.description}` : ""
                }`
              : "no longer listed"}
          </dd>
        </dl>

        <p className="text-xs text-muted-foreground" data-testid="skill-update-no-body">
          The skill&apos;s full text isn&apos;t shown — the host doesn&apos;t serve it, so this
          compares what it does and which revision it came from.
        </p>

        <DialogFooter>
          <Button
            variant="ghost"
            onClick={() => onOpenChange(false)}
            disabled={busy}
            data-testid="skill-update-keep"
          >
            Keep
          </Button>
          <Button onClick={() => void apply()} disabled={busy} data-testid="skill-update-confirm">
            {busy && <Loader2 className="mr-1.5 size-4 animate-spin" />}
            Update
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
