// The cards/list switch, shared by the Installed and Registry tabs so the two
// cannot drift into offering different drawings of the same kind of object.

import { LayoutGrid, Rows3 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { SKILL_LIST_VIEWS, type SkillListView } from "@/lib/skills-list";
import { cn } from "@/lib/utils";

const ICONS = { cards: LayoutGrid, list: Rows3 } as const;
const LABELS = { cards: "Cards", list: "List" } as const;

/** The cards/list switch above a skills list. */
export function SkillViewToggle({
  view,
  onView,
}: {
  view: SkillListView;
  onView: (next: SkillListView) => void;
}) {
  return (
    <div
      className="flex items-center gap-0.5 rounded-md border p-0.5"
      data-testid="skills-view-toggle"
    >
      {SKILL_LIST_VIEWS.map((option) => {
        const Icon = ICONS[option];
        const on = view === option;
        return (
          <Button
            key={option}
            type="button"
            variant="ghost"
            size="icon"
            aria-pressed={on}
            aria-label={`${LABELS[option]} view`}
            title={`${LABELS[option]} view`}
            data-testid={`skills-view-${option}`}
            className={cn(
              "size-7 rounded-sm",
              on ? "bg-muted text-foreground" : "text-muted-foreground",
            )}
            onClick={() => onView(option)}
          >
            <Icon className="size-4" />
          </Button>
        );
      })}
    </div>
  );
}
