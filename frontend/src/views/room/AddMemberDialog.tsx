import { useState } from "react";
import { Loader2 } from "lucide-react";

import type { OpenCompanyClient } from "@/api/client";
import { AvatarPicker } from "@/components/avatar-picker";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { AGENT_FIELDS } from "@/lib/agent";

/** The shared spec for `description`, so this form and the edit form agree. */
const DESCRIPTION_FIELD = AGENT_FIELDS.find((f) => f.key === "description")!;

export interface NewMemberFields {
  name: string;
  role: string;
  /** What this teammate does, in a line. Blank is allowed. */
  description: string;
  /** The standing instructions this teammate is born with. Blank from here. */
  instructions?: string;
  /**
   * The face, chosen before the teammate exists.
   *
   * `addTeamMember` takes no avatar, so a caller writes it as a second call
   * once the host has answered with an id — best-effort, because a teammate
   * with the wrong face is still a teammate.
   */
  avatar?: string;
  /**
   * Land on the new teammate's page, rather than staying where the dialog was
   * opened from.
   *
   * Always set by this dialog now: it collects the essentials, so everything else
   * about the teammate is filled in on the page this opens. A caller whose
   * write fell back to a local-only row has no id to navigate to and may
   * ignore it.
   */
  landOnProfile?: boolean;
}

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /**
   * Writes the teammate, answering whether the write landed.
   *
   * Awaited, and the dialog is cleared only on `true`: a 5xx that cleared the
   * form would throw away what the operator typed for no reason, so `false`
   * keeps it and Create becomes a retry.
   */
  onAdd: (fields: NewMemberFields) => boolean | Promise<boolean>;
  /** For the avatar picker's upload route. This dialog writes nothing itself. */
  client: OpenCompanyClient;
  company: string | null;
}

/**
 * Add an agent: a name, a face, a post, and what they do. Reached from the chat
 * pane's member list, the org chart's desk cards, and the roster.
 *
 * # Why it collects what it does
 *
 * It used to be the whole teammate — name, role, description, persona
 * instructions and an inbox switch — with a copilot that would design all of it
 * from one sentence, and a hand-over to the long form when that design was
 * refused. Four states in a create dialog, three of which existed to recover
 * from the other one.
 *
 * A teammate is not finished at the moment it is created, and this dialog was
 * the only place pretending otherwise. What it needs is enough to make a real
 * record the operator can then open: who they are, what they look like on a
 * roster of thirteen, what post they hold, and what they actually do. The
 * persona, the tools and the model are on the teammate's own page, next to the
 * copilot that drafts them and the record they are grounded in.
 *
 * The description is optional but asked for here, because it is the line every
 * other surface shows under the name — and the one the teammate is introduced
 * by. Its label and placeholder come from `AGENT_FIELDS`, so this form and the
 * edit form cannot describe the same field differently.
 */
export function AddMemberDialog({ open, onOpenChange, onAdd, client, company }: Props) {
  const [name, setName] = useState("");
  const [role, setRole] = useState("");
  /** `undefined` is the hashed mascot — a face nobody chose is still a face. */
  const [avatar, setAvatar] = useState<string | undefined>(undefined);
  const [description, setDescription] = useState("");
  const [creating, setCreating] = useState(false);

  function reset() {
    setName("");
    setRole("");
    setDescription("");
    setAvatar(undefined);
  }

  // Both required: a nameless agent is unrecognisable on the roster, and the
  // post is what the host derives the starting tool belt from.
  const ready = name.trim() !== "" && role.trim() !== "";

  async function submit() {
    if (!ready || creating) return;
    setCreating(true);
    let landed: boolean;
    try {
      landed = await onAdd({
        name: name.trim(),
        role: role.trim(),
        description: description.trim(),
        instructions: "",
        avatar,
        landOnProfile: true,
      });
    } catch {
      // A parent that rejected rather than answering. Read as "did not land",
      // which keeps the form for a retry — and caught rather than left to
      // escape, because `submit` is invoked as `void submit()`.
      landed = false;
    } finally {
      setCreating(false);
    }
    if (landed) reset();
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(o) => {
        // A write in flight holds the dialog: closing it now would leave the
        // operator where they were while a teammate they cannot see is created.
        if (creating) return;
        if (!o) reset();
        onOpenChange(o);
      }}
    >
      <DialogContent className="sm:max-w-md" showCloseButton={!creating}>
        <DialogHeader>
          <DialogTitle>Add agent</DialogTitle>
          <DialogDescription>
            Name them, give them a post, and say what they do. You can fill in the rest on their page.
          </DialogDescription>
        </DialogHeader>

        <div className="grid gap-4">
          <div className="grid gap-2">
            <Label htmlFor="agent-add-name">Name</Label>
            <Input
              id="agent-add-name"
              value={name}
              disabled={creating}
              placeholder="e.g. Ada"
              onChange={(e) => setName(e.target.value)}
              data-testid="team-add-name"
            />
          </div>

          <div className="grid gap-2">
            <Label>Icon</Label>
            {/* Seeded on the name, so the default mascot is the same face the
                roster would hash for this agent — the picker opens showing what
                you get if you choose nothing, rather than a stand-in that
                changes the moment the record exists. */}
            <AvatarPicker
              client={client}
              company={company}
              value={avatar}
              seed={name.trim() || "new-agent"}
              name={name.trim() || "New agent"}
              onChange={setAvatar}
              disabled={creating}
            />
          </div>

          <div className="grid gap-2">
            <Label htmlFor="agent-add-role">Post</Label>
            <Input
              id="agent-add-role"
              value={role}
              disabled={creating}
              placeholder="e.g. Research analyst"
              onChange={(e) => setRole(e.target.value)}
              data-testid="team-add-role"
            />
            <p className="text-xs text-muted-foreground">
              The company gives an agent its starting tools from this.
            </p>
          </div>

          <div className="grid gap-2">
            <Label htmlFor="agent-add-description">{DESCRIPTION_FIELD.label}</Label>
            <Textarea
              id="agent-add-description"
              value={description}
              disabled={creating}
              rows={3}
              placeholder={DESCRIPTION_FIELD.placeholder}
              onChange={(e) => setDescription(e.target.value)}
              data-testid="team-add-description"
            />
          </div>
        </div>

        <DialogFooter>
          <Button
            variant="ghost"
            disabled={creating}
            onClick={() => {
              reset();
              onOpenChange(false);
            }}
          >
            Cancel
          </Button>
          <Button
            disabled={!ready || creating}
            onClick={() => void submit()}
            data-testid="team-add-submit"
          >
            {creating && <Loader2 className="mr-1.5 size-4 animate-spin" />}
            Add agent
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
