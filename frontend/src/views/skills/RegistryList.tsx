// The Registry tab: what a company could add, searched and drawn as cards or
// rows.
//
// It carries its own search rather than sharing the Installed tab's: a query
// typed while browsing what could be added must not silently hide half of what
// already is. The cards/list switch is the shared one, so the two tabs draw the
// same kind of object the same way.

import { Check, Download, Search, Sparkles } from "lucide-react";

import type { RegistrySkill } from "@/api/skills";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { categoryStyle, registryEmptyLabel } from "@/lib/skills";
import { type SkillListView } from "@/lib/skills-list";
import { cn } from "@/lib/utils";
import { SkillsEmpty } from "@/views/skills/SkillsEmpty";
import { SkillViewToggle } from "@/views/skills/SkillViewToggle";

const HEADERS = ["Skill", "Category", "Publisher", ""];

export function RegistryList({
  skills,
  visible,
  installedIds,
  canManage,
  loading,
  error,
  query,
  onQuery,
  view,
  onView,
  onInstall,
}: {
  /** Everything the host serves, for telling "no registry" from "no match". */
  skills: RegistrySkill[];
  /** What the query leaves, in the order to draw them. */
  visible: RegistrySkill[];
  installedIds: Set<string>;
  canManage: boolean;
  loading: boolean;
  error: string | null;
  query: string;
  onQuery: (next: string) => void;
  view: SkillListView;
  onView: (next: SkillListView) => void;
  onInstall: (skill: RegistrySkill) => void;
}) {
  return (
    <>
      <div className="flex flex-wrap items-center gap-2">
        <div className="relative sm:max-w-xs sm:flex-1">
          <Search className="absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={query}
            onChange={(e) => onQuery(e.target.value)}
            placeholder="Search the registry…"
            className="pl-8"
            data-testid="registry-search"
          />
        </div>
        <div className="ml-auto">
          <SkillViewToggle view={view} onView={onView} />
        </div>
      </div>

      {error && (
        <Alert variant="destructive">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}

      {loading ? (
        <div className="grid gap-3 sm:grid-cols-2">
          <Skeleton className="h-32 rounded-xl" />
          <Skeleton className="h-32 rounded-xl" />
        </div>
      ) : visible.length === 0 ? (
        // A failed read leaves `skills` empty too, so the label must not derive
        // "serves no registry" from the same failure the alert above already
        // reports. The decider keeps the three cases apart.
        <SkillsEmpty
          label={registryEmptyLabel(error !== null, skills.length === 0)}
        />
      ) : view === "list" ? (
        <RegistryTable
          skills={visible}
          installedIds={installedIds}
          canManage={canManage}
          onInstall={onInstall}
        />
      ) : (
        <div className="grid gap-3 sm:grid-cols-2">
          {visible.map((s) => (
            <RegistryCard
              key={s.id}
              skill={s}
              installed={installedIds.has(s.id)}
              canManage={canManage}
              onInstall={() => onInstall(s)}
            />
          ))}
        </div>
      )}
    </>
  );
}

function RegistryTable({
  skills,
  installedIds,
  canManage,
  onInstall,
}: {
  skills: RegistrySkill[];
  installedIds: Set<string>;
  canManage: boolean;
  onInstall: (skill: RegistrySkill) => void;
}) {
  return (
    <div className="overflow-x-auto rounded-lg border border-border">
      <table className="w-full min-w-[42rem] border-collapse text-sm">
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
            <tr key={skill.id} data-testid="registry-row">
              <td className="border-b border-border py-3 pr-3 pl-4">
                <p className="font-medium">{skill.name}</p>
                <p className="line-clamp-1 text-xs text-muted-foreground">
                  {skill.description}
                </p>
              </td>
              <td className="border-b border-border px-3 py-3">
                {skill.category ? (
                  <Badge
                    variant="outline"
                    className={cn("capitalize", categoryStyle(skill.category))}
                  >
                    {skill.category}
                  </Badge>
                ) : null}
              </td>
              <td className="border-b border-border px-3 py-3">
                <span className="text-xs whitespace-nowrap text-muted-foreground">
                  {skill.publisher}
                  {skill.version ? ` · v${skill.version}` : ""}
                </span>
              </td>
              <td className="border-b border-border py-3 pr-4 pl-3">
                <div className="flex justify-end">
                  <InstallControl
                    installed={installedIds.has(skill.id)}
                    canManage={canManage}
                    onInstall={() => onInstall(skill)}
                  />
                </div>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function RegistryCard({
  skill,
  installed,
  canManage,
  onInstall,
}: {
  skill: RegistrySkill;
  installed: boolean;
  canManage: boolean;
  onInstall: () => void;
}) {
  return (
    <Card data-testid="registry-card">
      <CardContent className="space-y-2">
        <div className="flex items-center gap-2">
          <Sparkles className="size-4 text-muted-foreground" />
          <p className="font-medium">{skill.name}</p>
        </div>
        <p className="text-sm text-muted-foreground">{skill.description}</p>
        <div className="flex items-center justify-between pt-1">
          <div className="flex items-center gap-2">
            <Badge
              variant="outline"
              className={cn("capitalize", categoryStyle(skill.category))}
            >
              {skill.category}
            </Badge>
            <span className="text-xs text-muted-foreground">
              {skill.publisher}
              {skill.version ? ` · v${skill.version}` : ""}
            </span>
          </div>
          <InstallControl
            installed={installed}
            canManage={canManage}
            onInstall={onInstall}
          />
        </div>
      </CardContent>
    </Card>
  );
}

/** Shared so a row and a card cannot disagree about who may install what. */
function InstallControl({
  installed,
  canManage,
  onInstall,
}: {
  installed: boolean;
  canManage: boolean;
  onInstall: () => void;
}) {
  if (installed) {
    return (
      <span className="inline-flex items-center gap-1 text-xs font-medium text-status-done-text">
        <Check className="size-3.5" /> Installed
      </span>
    );
  }
  if (!canManage) return null;
  return (
    <Button variant="outline" size="sm" onClick={onInstall}>
      <Download className="size-4" /> Install
    </Button>
  );
}
