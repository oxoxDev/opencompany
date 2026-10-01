// The installed set as rows, for a company with more skills than a card grid
// can be scanned.
//
// The same columns MCP's own list settled on — what it is, where it came from,
// what state it is in — because the two are the same kind of answer about the
// same kind of object, and an operator who has learned to read one should not
// have to learn the other.
//
// Every control here carries the test id its card equivalent carries, so a spec
// can drive either rendering.

import { Sparkles } from "lucide-react";

import type { Skill } from "@/api/skills";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { categoryStyle } from "@/lib/skills";
import {
  skillDriftLabel,
  skillLastEditedLabel,
  skillSourceLabel,
} from "@/lib/skills-list";
import { cn } from "@/lib/utils";

const HEADERS = ["Skill", "Source", "Edited", ""];

export function SkillsTable({
  skills,
  canManage,
  now,
  onToggle,
  onUninstall,
  onUpdate,
  onOpen,
  renderMenu,
}: {
  skills: Skill[];
  canManage: boolean;
  now: number;
  onToggle: (skill: Skill) => void;
  onUninstall: (skill: Skill) => void;
  onUpdate: (skill: Skill) => void;
  onOpen: (skill: Skill) => void;
  /** The row menu, passed in so both renderings share one definition of it. */
  renderMenu: (skill: Skill) => React.ReactNode;
}) {
  return (
    <div className="overflow-x-auto rounded-lg border border-border">
      <table className="w-full min-w-[46rem] border-collapse text-sm">
        <thead>
          <tr>
            {HEADERS.map((head, i) => (
              <th
                key={head || `spacer-${i}`}
                className="border-b border-border px-3 py-2.5 text-left text-3xs font-medium tracking-wide text-muted-foreground uppercase first:pl-4 last:pr-4"
              >
                {head}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {skills.map((skill) => (
            <SkillRow
              key={skill.id}
              skill={skill}
              canManage={canManage}
              now={now}
              onToggle={() => onToggle(skill)}
              onUninstall={() => onUninstall(skill)}
              onUpdate={() => onUpdate(skill)}
              onOpen={() => onOpen(skill)}
              menu={renderMenu(skill)}
            />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function SkillRow({
  skill,
  canManage,
  now,
  onToggle,
  onOpen,
  menu,
}: {
  skill: Skill;
  canManage: boolean;
  now: number;
  onToggle: () => void;
  onUninstall: () => void;
  onUpdate: () => void;
  onOpen: () => void;
  menu: React.ReactNode;
}) {
  const drift = skillDriftLabel(skill);
  const category = skill.category?.trim();

  return (
    <tr
      data-testid="installed-row"
      className={cn("align-middle", !skill.enabled && "opacity-70")}
    >
      <td className="border-b border-border py-3 pr-3 pl-4">
        <div className="flex min-w-0 items-start gap-2">
          <Sparkles className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
          <div className="min-w-0">
            {/* The name is the link, the way it is on an MCP row: a row that
                opens a page does not need a control saying so. */}
            <button
              type="button"
              onClick={onOpen}
              data-testid="skill-card-open"
              className="block max-w-full truncate rounded-sm text-left font-medium hover:underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
            >
              {skill.name}
            </button>
            <span className="block max-w-[30rem] truncate text-xs text-muted-foreground">
              {skill.description}
            </span>
          </div>
        </div>
      </td>
      <td className="border-b border-border px-3 py-3">
        <div className="flex flex-wrap items-center gap-1.5">
          <span
            data-testid="skill-source"
            className="text-xs text-muted-foreground"
          >
            {skillSourceLabel(skill)}
          </span>
          {drift ? (
            <Badge
              variant="outline"
              data-testid={
                skill.modified ? "skill-modified" : "skill-update-available"
              }
              className="bg-status-blocked-soft text-status-blocked-text"
            >
              {drift}
            </Badge>
          ) : null}
          {category ? (
            <Badge
              variant="outline"
              data-testid="skill-category"
              className={cn("capitalize", categoryStyle(category))}
            >
              {category}
            </Badge>
          ) : null}
        </div>
      </td>
      <td className="border-b border-border px-3 py-3">
        <span
          data-testid="skill-last-edited"
          className="text-xs whitespace-nowrap text-muted-foreground"
        >
          {skillLastEditedLabel(skill.updatedAtMillis, now)}
        </span>
      </td>
      <td className="border-b border-border py-3 pr-4 pl-3">
        <div className="flex items-center justify-end gap-1">
          <Switch
            checked={skill.enabled}
            disabled={!canManage}
            onCheckedChange={canManage ? onToggle : undefined}
            aria-label="Enable skill"
          />
          {canManage && menu}
        </div>
      </td>
    </tr>
  );
}
