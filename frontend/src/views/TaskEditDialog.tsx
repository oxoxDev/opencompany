// The task edit form, extracted from the board (#184) so both the Kanban board
// and the Task Detail screen open the same dialog without a circular import.
// This is an *edit* form — title / note / column / priority / assignee plus a
// delete — unchanged from its original home on the board screen (retired in
// issue #1140; the board is the `tasks` ledger's columns now).

import { useEffect, useState } from "react";
import { Loader2, Trash2 } from "lucide-react";

import {
  deleteTask,
  patchTask,
  type PatchTask,
  type Task,
  type TaskDeliverable,
} from "@/api/tasks";
import type { OpenCompanyClient } from "@/api/client";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { computeTaskPatch } from "@/lib/task-edit";
import { labelFor } from "@/lib/board-columns";
import { useBoardColumns } from "@/hooks/use-board-columns";
import { toast } from "sonner";
import { AssigneeSelect } from "./AssigneeSelect";

const PRIORITIES = ["low", "medium", "high"] as const;

/** The once-vs-workflow options, in review order (issue #580). */
const DELIVERABLES: { value: TaskDeliverable; label: string }[] = [
  { value: "once", label: "Do it once" },
  { value: "workflow", label: "Build me the workflow" },
];

/**
 * The columns where the deliverable can still be flipped (issue #580).
 *
 * Once a card leaves To-do/Planning the choice is settled: the builder pass
 * fires on the drag into In Progress, so changing once-vs-workflow afterwards
 * cannot rebuild what already ran. The control is disabled there rather than
 * hidden — an honest "locked" reads better than a field that silently vanishes —
 * but this is a **UI-honesty** guard, not enforcement: the host is the authority
 * on whether a late patch is accepted, and the untouched-field diff below means
 * a save that does not touch the deliverable never sends it anyway.
 */
const DELIVERABLE_EDITABLE = new Set(["todo", "planning"]);

/**
 * Edit a card (or delete it). Open when `task` is non-null; `onClose` fires on
 * dismiss, `onSaved`/`onDeleted` hand the reconciled row back to the caller so
 * the board or detail screen can update its own state.
 */
export function TaskEditDialog({
  task,
  onClose,
  onSaved,
  onDeleted,
  client,
  company,
}: {
  task: Task | null;
  onClose: () => void;
  onSaved: (t: Task) => void;
  onDeleted: (id: string) => void;
  client: OpenCompanyClient;
  company: string | null;
}) {
  // From the `tasks` ledger, so this select can never offer a column the host's
  // write boundary would refuse — which is what a second, local list allowed.
  const columns = useBoardColumns(client, company);
  const [draft, setDraft] = useState<PatchTask>({});
  const [busy, setBusy] = useState(false);

  // Reset the edit draft each time a different card is opened.
  useEffect(() => {
    if (task) {
      setDraft({
        title: task.title,
        note: task.note ?? "",
        column: task.column,
        priority: task.priority,
        assignee: task.assignee,
        // Absent means `"once"` on the wire, so the control is seeded with the
        // normalized value and a card with no stored deliverable edits as the
        // one-off it is (issue #580).
        deliverable: task.deliverable ?? "once",
      });
    }
  }, [task]);

  if (!task) return null;

  async function save() {
    if (!task) return;
    // Only the fields the operator actually touched (issue #263's roster-safety
    // diff, extended with #580's deliverable). See `computeTaskPatch`.
    const patch = computeTaskPatch(draft, task);
    if (Object.keys(patch).length === 0) {
      // Nothing to write. Saying so beats a round-trip that reports "Saved."
      // for an edit that never happened.
      toast.success("No changes to save.");
      onSaved(task);
      return;
    }
    setBusy(true);
    try {
      const saved = await patchTask(client, company, task.id, patch);
      onSaved(saved);
      toast.success("Saved.");
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "could not save");
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    if (!task) return;
    setBusy(true);
    try {
      await deleteTask(client, company, task.id);
      onDeleted(task.id);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "could not delete");
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={!!task} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>Edit task</DialogTitle>
          <DialogDescription>
            Edit the card, or drop it into “In progress” on the board to dispatch it.
          </DialogDescription>
        </DialogHeader>

        <div className="grid gap-3">
          <div className="grid gap-1.5">
            <Label htmlFor="task-title">Title</Label>
            <Input
              id="task-title"
              value={draft.title ?? ""}
              onChange={(e) => setDraft((d) => ({ ...d, title: e.target.value }))}
            />
          </div>

          <div className="grid gap-1.5">
            <Label htmlFor="task-note">Note / result</Label>
            <Textarea
              id="task-note"
              rows={8}
              className="font-mono text-xs"
              value={draft.note ?? ""}
              onChange={(e) => setDraft((d) => ({ ...d, note: e.target.value }))}
            />
          </div>

          <div className="grid grid-cols-3 gap-3">
            <div className="grid gap-1.5">
              <Label htmlFor="task-column">Column</Label>
              {/* Fall back to the card's own value rather than `undefined`.
                  `draft` starts empty and is seeded a tick later by the effect
                  above, so a bare `draft.column` hands Base UI `undefined` on
                  the first render — which latches the select as *uncontrolled*
                  and makes it ignore the seeded value, leaving the trigger
                  blank for the whole life of the dialog. */}
              <Select
                value={draft.column ?? task.column}
                onValueChange={(v) => setDraft((d) => ({ ...d, column: v ?? undefined }))}
              >
                <SelectTrigger id="task-column">
                  {/* The trigger renders the raw value unless told otherwise,
                      and a column's id is not its label (`in_progress` vs "In
                      progress"). */}
                  <SelectValue>
                    {(selected) =>
                      selected ? labelFor(columns, String(selected)) : ""
                    }
                  </SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {columns.map((c) => (
                    <SelectItem key={c.id} value={c.id}>
                      {c.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="task-priority">Priority</Label>
              {/* Same seeding hazard as Column above. A priority is its own
                  label, so only the casing the items carry needs restating. */}
              <Select
                value={draft.priority ?? task.priority}
                onValueChange={(v) => setDraft((d) => ({ ...d, priority: v ?? undefined }))}
              >
                <SelectTrigger id="task-priority">
                  <SelectValue className="capitalize" />
                </SelectTrigger>
                <SelectContent>
                  {PRIORITIES.map((p) => (
                    <SelectItem key={p} value={p} className="capitalize">
                      {p}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            {/* `min-w-0`: a grid item's automatic minimum size is its content's
                min-content width, so the assignee's long ids would otherwise
                widen this track and squeeze Column and Priority away. */}
            <div className="grid min-w-0 gap-1.5">
              <Label htmlFor="task-assignee">Assignee</Label>
              {/* Issue #263: picked from the roster, not typed. An assignee the
                  roster no longer carries still renders — flagged — so a save
                  that does not touch it can never quietly rewrite it. */}
              <AssigneeSelect
                id="task-assignee"
                client={client}
                company={company}
                value={draft.assignee ?? ""}
                onChange={(next) => setDraft((d) => ({ ...d, assignee: next }))}
              />
            </div>
          </div>

          <div className="grid gap-1.5">
            <Label htmlFor="task-deliverable">Deliverable</Label>
            <Select
              value={draft.deliverable ?? task.deliverable ?? "once"}
              onValueChange={(v) =>
                setDraft((d) => ({ ...d, deliverable: (v as TaskDeliverable) ?? undefined }))
              }
              disabled={!DELIVERABLE_EDITABLE.has(task.column)}
            >
              <SelectTrigger id="task-deliverable" data-testid="edit-deliverable">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {DELIVERABLES.map((d) => (
                  <SelectItem key={d.value} value={d.value}>
                    {d.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {!DELIVERABLE_EDITABLE.has(task.column) && (
              <p className="text-2xs text-muted-foreground">
                Locked once work starts — the workflow is built when a card enters In progress, so
                this can only be changed while it&apos;s still in To-do or Planning.
              </p>
            )}
          </div>
        </div>

        <DialogFooter className="justify-between sm:justify-between">
          <AlertDialog>
            <AlertDialogTrigger
              render={
                <Button variant="ghost" size="sm" disabled={busy}>
                  <Trash2 className="mr-1.5 size-4" />
                  Delete
                </Button>
              }
            />
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Delete “{task.title}”?</AlertDialogTitle>
                <AlertDialogDescription>
                  This permanently removes the task and can’t be undone.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Keep task</AlertDialogCancel>
                <AlertDialogAction
                  onClick={() => void remove()}
                  className="bg-destructive text-white hover:bg-destructive/90"
                >
                  Delete task
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
          <div className="flex gap-2">
            <Button variant="outline" onClick={onClose} disabled={busy}>
              Cancel
            </Button>
            <Button onClick={() => void save()} disabled={busy}>
              {busy && <Loader2 className="mr-1.5 size-4 animate-spin" />}
              Save
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
