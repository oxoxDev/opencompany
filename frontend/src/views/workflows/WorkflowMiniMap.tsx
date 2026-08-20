// A minimap for the workflow canvas that replaces React Flow's built-in
// `<MiniMap>` (issue #1259).
//
// The built-in component's own bounding-box math — `viewScale =
// Math.max(scaledWidth, scaledHeight)` over `getBoundsOfRects(nodeBounds,
// viewBB)` — is not configurable via any prop, and `viewBB` is the CURRENT
// pan/zoom viewport, unioned in unconditionally. For a linear, low-vertical-
// spread workflow (every shipped company template: one node per depth layer),
// the on-screen canvas itself ends up zoomed out to fit the graph's width,
// leaving a viewport rect far taller than any node — so no matter what
// container size the built-in minimap was given, ITS bounding box stayed
// dominated by that viewport height, not the actual node height, and every
// node rect kept rendering as an imperceptible sliver. See `graph.ts`'s
// `NODE_MIN_VISIBLE_FRACTION` doc comment for the full trail, including why
// shrinking the built-in component's `style` alone (the first fix attempted
// here) measured as no improvement in a real browser.
//
// This component computes its own scale from `contentBounds` — node
// positions only, never the live viewport — so it isn't subject to that
// coupling. It renders the same DOM shape (`react-flow__minimap` /
// `-svg` / `-mask` / `-node` classes, `data-testid="rf__minimap"`) as the
// built-in component, so the imported `@xyflow/react/dist/style.css` themes
// it identically in both color modes with no new CSS.
//
// Panning/zooming from the minimap is reimplemented on top of `useReactFlow`
// (`setCenter`, `zoomIn`/`zoomOut`) — the PUBLIC, documented API — rather than
// reusing the built-in component's internal `XYMinimap` drag/wheel wiring,
// which lives in `@xyflow/system`. That package isn't a direct dependency of
// this app (only a transitive one of `@xyflow/react`), so importing from it
// would work by accident of hoisting rather than by contract.

import { useCallback, useRef } from "react";
import {
  MiniMapNode,
  Panel,
  useReactFlow,
  useStore,
  useViewport,
  type Node,
} from "@xyflow/react";

import type { WorkflowNodeData } from "@/lib/workflow-sample";
import { cn } from "@/lib/utils";
import {
  contentBounds,
  minimapDimensions,
  minimapViewBox,
  NODE_H,
  NODE_W,
} from "./graph";

/** Matches the built-in `<MiniMap>`'s default — padding around the content so
 * nodes at the very edge aren't drawn flush against the minimap's border.
 * Kept in sync with the identical constant in `graph.ts`'s `minimapViewBox`,
 * which computes the box this pads; only the mask outline (drawn slightly
 * larger than the viewBox so it covers to the edge with no seam) needs it
 * again here. */
const OFFSET_SCALE = 5;

export interface WorkflowMiniMapProps {
  nodes: Node<WorkflowNodeData>[];
  className?: string;
}

export function WorkflowMiniMap({ nodes, className }: WorkflowMiniMapProps) {
  const svgRef = useRef<SVGSVGElement>(null);
  const reactFlowInstance = useReactFlow<Node<WorkflowNodeData>>();
  const { x: viewX, y: viewY, zoom } = useViewport();
  const paneWidth = useStore((s) => s.width);
  const paneHeight = useStore((s) => s.height);

  const bounds = contentBounds(nodes);
  const hasContent = bounds.width > 0 && bounds.height > 0;
  const { width: containerWidth, height: containerHeight } =
    minimapDimensions(nodes);
  const {
    x: vbX,
    y: vbY,
    width: vbWidth,
    height: vbHeight,
  } = minimapViewBox(nodes);
  const viewScale = Math.max(vbWidth / containerWidth, vbHeight / containerHeight);
  const offset = OFFSET_SCALE * viewScale;

  // The current pan/zoom viewport, in graph coordinates — drawn as the mask's
  // cutout, exactly like the built-in component's `viewBB`. Unlike the
  // built-in component, this never feeds back into the scale above (see
  // `minimapViewBox`'s doc comment for why that distinction is the fix).
  const viewportX = -viewX / zoom;
  const viewportY = -viewY / zoom;
  const viewportWidth = paneWidth / zoom;
  const viewportHeight = paneHeight / zoom;

  const maskX = vbX - offset;
  const maskY = vbY - offset;
  const maskWidth = vbWidth + offset * 2;
  const maskHeight = vbHeight + offset * 2;
  const maskPath = `M${maskX},${maskY}h${maskWidth}v${maskHeight}h${-maskWidth}z M${viewportX},${viewportY}h${viewportWidth}v${viewportHeight}h${-viewportWidth}z`;

  // Maps a pointer position (CSS pixels, relative to the svg element) to
  // graph coordinates, then recenters the main canvas there — click jumps,
  // and holding the pointer down while moving drags the view, matching the
  // built-in component's `pannable`.
  const panTo = useCallback(
    (clientX: number, clientY: number) => {
      const svg = svgRef.current;
      if (!svg || !hasContent) return;
      const rect = svg.getBoundingClientRect();
      if (rect.width === 0 || rect.height === 0) return;
      const px = (clientX - rect.left) / rect.width;
      const py = (clientY - rect.top) / rect.height;
      const gx = vbX + px * vbWidth;
      const gy = vbY + py * vbHeight;
      void reactFlowInstance.setCenter(gx, gy, {
        zoom: reactFlowInstance.getZoom(),
        duration: 0,
      });
    },
    [hasContent, reactFlowInstance, vbX, vbY, vbWidth, vbHeight],
  );

  const draggingRef = useRef(false);

  const onPointerDown = useCallback(
    (e: React.PointerEvent<SVGSVGElement>) => {
      draggingRef.current = true;
      e.currentTarget.setPointerCapture(e.pointerId);
      panTo(e.clientX, e.clientY);
    },
    [panTo],
  );

  const onPointerMove = useCallback(
    (e: React.PointerEvent<SVGSVGElement>) => {
      if (!draggingRef.current) return;
      panTo(e.clientX, e.clientY);
    },
    [panTo],
  );

  const endDrag = useCallback((e: React.PointerEvent<SVGSVGElement>) => {
    draggingRef.current = false;
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
  }, []);

  const onWheel = useCallback(
    (e: React.WheelEvent<SVGSVGElement>) => {
      e.preventDefault();
      if (e.deltaY < 0) {
        void reactFlowInstance.zoomIn({ duration: 0 });
      } else if (e.deltaY > 0) {
        void reactFlowInstance.zoomOut({ duration: 0 });
      }
    },
    [reactFlowInstance],
  );

  return (
    <Panel
      position="bottom-right"
      className={cn("react-flow__minimap", className)}
      data-testid="rf__minimap"
    >
      <svg
        ref={svgRef}
        width={containerWidth}
        height={containerHeight}
        viewBox={`${vbX} ${vbY} ${vbWidth} ${vbHeight}`}
        className="react-flow__minimap-svg"
        role="img"
        aria-label="Mini Map"
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
        onWheel={onWheel}
        style={{ cursor: "pointer" }}
      >
        {nodes.map((n) => (
          <MiniMapNode
            key={n.id}
            id={n.id}
            x={n.position.x}
            y={n.position.y}
            width={n.initialWidth ?? NODE_W}
            height={n.initialHeight ?? NODE_H}
            borderRadius={5}
            className=""
            shapeRendering="crispEdges"
            selected={false}
          />
        ))}
        <path
          className="react-flow__minimap-mask"
          d={maskPath}
          fillRule="evenodd"
          pointerEvents="none"
        />
      </svg>
    </Panel>
  );
}
