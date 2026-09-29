import { useMemo } from "react";
import { Users } from "lucide-react";

import { TeammateAvatar } from "@/components/teammate-avatar";
import { cn } from "@/lib/utils";
import {
  NODE_HEIGHT,
  NODE_WIDTH,
  centreOf,
  edgePath,
  edgeWeight,
  layoutComms,
} from "@/views/comms/layout";
import type { CommsEdge, CommsGraph } from "@/views/comms/model";

/**
 * The company's own wiring, drawn.
 *
 * Hand-drawn SVG under absolutely-positioned HTML rather than a graph library,
 * for two reasons that are not "avoid a dependency":
 *
 * - The nodes want real DOM — a teammate's avatar, a status ring, a seat count —
 *   and both libraries already in the bundle are wrong here. `d3-force` draws
 *   primitives and re-heats on insertion; `@xyflow/react` renders React nodes
 *   but brings a pan/zoom canvas this small, layered graph does not need.
 * - Every colour resolves through the design tokens, so the graph themes for
 *   free — the mistake the knowledge graph had to be retrofitted out of.
 *
 * All the maths is in `layout.ts` and is pure. This file positions and paints.
 */
export function CommsGraphView({
  graph,
  selected,
  onSelect,
  className,
}: {
  graph: CommsGraph;
  selected?: string | null;
  onSelect?: (id: string | null) => void;
  className?: string;
}) {
  const layout = useMemo(() => layoutComms(graph), [graph]);
  const at = useMemo(
    () => new Map(layout.placed.map((p) => [p.node.id, p])),
    [layout],
  );

  if (graph.nodes.length === 0) {
    return (
      <p className="rounded-lg border border-dashed border-border p-6 text-center text-sm text-muted-foreground">
        No teammates or desks yet.
      </p>
    );
  }

  return (
    <div className={cn("overflow-x-auto", className)}>
      <div
        className="relative"
        style={{ width: layout.width, height: layout.height }}
      >
        <svg
          className="absolute inset-0"
          width={layout.width}
          height={layout.height}
          aria-hidden
        >
          {graph.edges.map((edge) => {
            const from = at.get(edge.from);
            const to = at.get(edge.to);
            if (!from || !to) return null;
            const dimmed =
              selected != null && edge.from !== selected && edge.to !== selected;
            return (
              <path
                key={edge.id}
                d={edgePath(
                  centreOf(from),
                  centreOf(to),
                  from.node.kind === to.node.kind,
                )}
                fill="none"
                className={EDGE_CLASS[edge.kind]}
                strokeWidth={edgeWeight(edge.count)}
                strokeDasharray={STRUCTURAL.has(edge.kind) ? "4 4" : undefined}
                opacity={dimmed ? 0.15 : 1}
              />
            );
          })}
        </svg>

        {layout.placed.map(({ node, x, y }) => {
          const dimmed = selected != null && node.id !== selected;
          return (
            <button
              key={node.id}
              type="button"
              onClick={() => onSelect?.(node.id === selected ? null : node.id)}
              className={cn(
                "absolute flex items-center gap-2 rounded-lg border bg-card px-2 text-left transition-opacity",
                node.id === selected ? "border-primary" : "border-border",
                dimmed && "opacity-40",
              )}
              style={{ left: x, top: y, width: NODE_WIDTH, height: NODE_HEIGHT }}
            >
              {node.kind === "agent" ? (
                <TeammateAvatar
                  name={node.label}
                  // Seeded on the roster id, not the display name, so a face
                  // stays the same when somebody is renamed — the same rule the
                  // rest of the console follows.
                  tone={node.id.replace(/^agent:/, "")}
                  className="size-7"
                />
              ) : (
                <span className="flex size-7 items-center justify-center rounded-md bg-muted">
                  <Users aria-hidden className="size-3.5 text-muted-foreground" />
                </span>
              )}
              <span className="min-w-0 flex-1">
                <span className="flex items-center gap-1">
                  <span className="truncate text-xs font-medium">{node.label}</span>
                  {/*
                    A ring, not a colour: "currently taking a turn" is a status,
                    and status never rests on hue alone.
                  */}
                  {node.speaking ? (
                    <span
                      title="taking a turn"
                      className="size-1.5 shrink-0 animate-pulse rounded-full bg-status-running"
                    />
                  ) : null}
                </span>
                <span className="block truncate text-3xs text-muted-foreground">
                  {node.spawned ? "created at runtime" : node.detail}
                </span>
              </span>
            </button>
          );
        })}
      </div>

      <Legend />
    </div>
  );
}

/** Structure is dashed; history is solid. */
const STRUCTURAL = new Set(["member", "may-delegate"]);

/** Full class strings — Tailwind scans source text and never sees a template. */
const EDGE_CLASS: Record<CommsEdge["kind"], string> = {
  member: "stroke-border",
  "may-delegate": "stroke-muted-foreground/40",
  "handed-off": "stroke-primary/70",
  spawned: "stroke-status-done/70",
  spoke: "stroke-status-running/80",
};

function Legend() {
  return (
    <ul className="mt-3 flex flex-wrap gap-x-4 gap-y-1 text-2xs text-muted-foreground">
      <li>
        <Dash className="stroke-border" /> on the desk
      </li>
      <li>
        <Dash className="stroke-muted-foreground/40" /> may delegate to
      </li>
      <li>
        <Solid className="stroke-primary/70" /> handed work to
      </li>
      <li>
        <Solid className="stroke-status-done/70" /> created
      </li>
      <li>
        <Solid className="stroke-status-running/80" /> spoke to (broadcast, dm, referral)
      </li>
      <li className="text-muted-foreground/70">
        dashed is what the manifest allows; solid is what has happened
      </li>
    </ul>
  );
}

function Dash({ className }: { className: string }) {
  return (
    <svg width="18" height="6" className="inline-block align-middle" aria-hidden>
      <line x1="0" y1="3" x2="18" y2="3" strokeWidth="2" strokeDasharray="4 4" className={className} />
    </svg>
  );
}

function Solid({ className }: { className: string }) {
  return (
    <svg width="18" height="6" className="inline-block align-middle" aria-hidden>
      <line x1="0" y1="3" x2="18" y2="3" strokeWidth="2" className={className} />
    </svg>
  );
}
