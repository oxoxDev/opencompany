// The task edit form, extracted from the board (#184) so both the Kanban board
// and the Task Detail screen open the same dialog without a circular import.
// This is an *edit* form — title / note / column / priority / assignee plus a
// delete — unchanged from its original home in `TasksView`.

import { useEffect, useState } from "react";
import { Loader2, Trash2 } from "lucide-react";

import { deleteTask, patchTask, type PatchTask, type Task } from "@/api/tasks";
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
import { TASK_COLUMNS } from "@/lib/tasks-sample";
import { toast } from "sonner";

const PRIORITIES = ["low", "medium", "high"] as const;

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
      });
    }
  }, [task]);

  if (!task) return null;

  async function save() {
    if (!task) return;
    setBusy(true);
    try {
      const saved = await patchTask(client, company, task.id, draft);
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
              <Select
                value={draft.column}
                onValueChange={(v) => setDraft((d) => ({ ...d, column: v ?? undefined }))}
              >
                <SelectTrigger id="task-column">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {TASK_COLUMNS.map((c) => (
                    <SelectItem key={c.id} value={c.id}>
                      {c.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="task-priority">Priority</Label>
              <Select
                value={draft.priority}
                onValueChange={(v) => setDraft((d) => ({ ...d, priority: v ?? undefined }))}
              >
                <SelectTrigger id="task-priority">
                  <SelectValue />
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
            <div className="grid gap-1.5">
              <Label htmlFor="task-assignee">Assignee</Label>
              <Input
                id="task-assignee"
                value={draft.assignee ?? ""}
                placeholder="agent id"
                onChange={(e) => setDraft((d) => ({ ...d, assignee: e.target.value }))}
              />
            </div>
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
